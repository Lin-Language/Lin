# Changelog

All notable changes to Lin are documented here.
## [1.1.0](https://github.com/Lin-Language/Lin/compare/v1.0.0...v1.1.0) - 2026-07-10

### Bug Fixes

- **runtime**: Two frozen() crashes — int-keyed map keys + shared record-array shells
- **runtime**: Lin_rc_release missing IMMORTAL_RC guard (asymmetry with retain)
- **codegen**: NullableRecord vs null compared its refcount byte as a tag
- **ir**: CK.2 is-elision must use exact tag identity, not is_compatible widening
- **ir**: Add nonneg:false to CL.3 devirt Index sites (CK.1 field; semantic merge fix)
- **check**: Cross-module recursive union type hangs type-checker
- **ir**: Nested if/else literal-union record repr mismatch → segfault
- **ir**: Keep repr SumNode oracle strict; homogenization alone fixes litunion record
- **codegen**: Union combinator closure ABI mismatch → segfault
- **codegen**: Sum-union callback return stored unboxed → segfault
- **ir**: Gate REC-CPR is-T ptr-null fast path on target==inner record
- **codegen**: Sum-union callback return stored unboxed → segfault (v2)
- **ir**: RC leak in coerce_if_branch concrete-concrete arm (owned=is_rc_type)
- **ir**: Null-guarded FieldGet for {String:SealedRecord} map value field access
- **runtime**: Frozen 0xFD→0xFE repack size-uniformity guard + soundness proof (B3)
- **runtime**: Four RC/safety hardening fixes (B1)
- **codegen**: Repr-aware SumNode-field release in FieldSet + sealed-array-field guard
- **checker**: Is/match on union alias with record members uses structural check
- **runtime**: Drop unsound tagged_release TAG_FUNCTION arm (double-free on RAPTOR)
- **runtime**: CSV column projection — decode cols as raw LinArray, not TaggedVal
- **ir**: Substr-fuse must disqualify slice temps used as Index/IndexSet object or value
- **ir**: Getset-fuse must disqualify a call in the get->set window (dangling-slot UAF guard)
- **ir**: Empty-array guard in inline sealed-record sort
- **codegen,ir**: Adopt mimalloc-exposed RC/codegen fixes from fix/json-view-uaf
- **runtime**: Close double-free of stream transform closure on fault path
- **codegen**: Retain SumNode on sealed-record FieldSet when types structurally differ
- **codegen**: Match forward-decl signature for cross-module flat-union returns
- **compile**: Point parse errors at the imported file, not the entry file
- **check**: Reject nullable array index as compile-time type error
- **ir**: Exactly-once reclaim for sealed array/record aliased through AnyVal
- **ir**: Materialize packed-array-element views captured into closures
- **codegen**: Chained int-map index m[a][b] with a boxed key inside a closure
- **parse**: Index-signature record hint fires on newline-separated bare keys
- **check**: Coerce array literal to a union's tuple member; strip Null on nullable index-assign
- Union / nullable-record representation soundness (3 IR/codegen bugs)
- **ir**: Coerce flat scalar-nullable union at the closure/return ABI boundary
- **fmt**: Make formatting idempotent — blank line before chain-rooted statement dropped on re-format
- **fmt**: Preserve import-foreign inner comments + val-run = alignment
- **ir**: Partial application of generic functions crashed codegen
- **codegen**: Sumnode SumDesc kind-5 namespace violation + materializer double-free
- **runtime**: Content-verify heap-desc memo to prevent stale-descriptor UAF

### Documentation

- **perf**: Record take-five RELEASE outcome + measure-in-release lesson
- **perf**: Fold take-five findings into PERFORMANCE.md §5.9, delete the plan doc
- **perf**: §5.10 — records aren't maps; the RAPTOR wall is reference-record memory latency
- Compiler-cleanup campaign brief (soundness/RC/stdlib/AnyVal)
- **perf**: Correct AnyVal field-access mechanism (tag-dispatch + O(1) map_get, not O(n) scan)
- Record compiler-cleanup campaign outcome (WS-A/B/C/D merged; B1 false-positive; AnyVal verdict)
- Remove project-cleanup-compiler.md (campaign complete + merged)
- **perf**: §5.12 — string-interning keystone (-22%), RC-read, closure-devirt; the serializing-vs-overlapped cut line
- **perf**: §5.12 update — both structural lanes paid off (IntUnion -12%, inline-0xFD -3.3%)
- **perf**: Record nbody campaign (1319→358ms, 3.7×) + memory-latency wall
- **exercises**: Add 30 progressive AoC-style Lin exercises
- Fold ASYNC_DESIGN.md into SPECIFICATION.md §24
- **perf**: Update §5.13 nbody — final 319ms (4.1×) + sound-bounds-elision pays where crude probe didn't + data-ptr-hoist neutral
- Rewrite MEMORY_MANAGEMENT.md to match ground truth
- Rationalise ADRs — remove reversed/abandoned decisions, fix numbering note
- **perf**: Add §5.14 knucleotide 216→115ms (−47%) — substring + get-set map-key fusion
- **perf**: §5.15 RAPTOR GROUP 1881→1029ms — allocation was the lever, not map_get_int
- **perf**: §5.16 RC-elide borrowed FieldGet/Index — GROUP −5.1%, RANGE −4.4%
- **perf**: §5.17 RAPTOR scan-loop unboxing + row hoists — GROUP −11%, RANGE −9%; walk-skip neutral
- **exercises**: Single nav entry, self-contained stubs, bigger tests, de-themed

### Features

- **runtime**: LIN_VERIFY_REPR report-only record→map conversion verifier (Stage 1)
- **types**: Add Type::Frozen<T> — immutable-proof wrapper for sealed-array strided reads
- **stdlib**: Range accepts unsigned and wider integer bounds
- **stdlib**: Add findMap to std/iter

### Other

- Merge pull request #8 from Lin-Language/release-plz-2026-06-07T17-31-30Z

chore: release v1.0.0
- Clean up
- Update lin-agent
- Add new performance plan
- Merge branch 'master' into perf/range-for-fusion-fix
- Merge branch 'fix/crossmod-recursive-type-hang'
- Merge branch 'fix/litunion-record-repr'
- Merge branch 'refs/heads/master' into take5/integration3
- Merge branch 'fix/litunion-union-combinator-segfault'
- Merge branch 'fix/sumunion-map-box'
- Revert "Merge branch 'fix/sumunion-map-box'"

This reverts commit 42a3db88e7e18ee5f3c9250bae7d0b48a28ffffd, reversing
changes made to 260088d6ce5c1954f25697324c5dd4bebb3d9483.
- Merge branch 'fix/litunion-record-repr' into integ/examples-and-fixes
- Merge branch 'idiom/example-cleanups' into integ/examples-and-fixes
- Merge branch 'integ/examples-and-fixes' into examples/idiom-and-tidy
- Merge branch 'master' into examples/idiom-and-tidy
- Merge branch 'master' into examples/idiom-and-tidy
- Merge branch 'fix/sumunion-map-box-v2'
- Merge branch 'master' into examples/idiom-and-tidy
- Merge branch 'examples/idiom-and-tidy'
- Merge branch 'refs/heads/master' into take5/combo
- Merge branch 'refs/heads/master' into take5/combo
- Merge branch 'refs/heads/master' into take5/frozenrc
- Merge branch 'refs/heads/master' into take5/frozenrc
- Merge branch 'refs/heads/master' into take5/coercefin
- Merge branch 'refs/heads/master' into take5/stridespec
- Merge branch 'repr/s4b-E2' into repr/single-form
- Merge branch 'repr/s4b-E3' into repr/single-form
- Merge perf/csv-column-projection: recordRows column projection
- Merge docs/anyval-access-mechanism: correct AnyVal field-access mechanism
- **csv**: RecordRows returns Stream<{String:String}>
- Merge perf/type-csv-rows: recordRows returns Stream<{String:String}>
- Merge branch 'refs/heads/master' into cleanup/a1
- Merge branch 'refs/heads/master' into cleanup/b2
- Merge branch 'refs/heads/master' into cleanup/b1
- Merge perf/llvm-mem-attrs: structural LLVM memory-effect attributes for runtime fns
- Merge branch 'refs/heads/master' into cleanup/c
- Merge branch 'refs/heads/master' into cleanup/b1
- Merge branch 'refs/heads/master' into cleanup/b3
- Merge master (csv type refinement + llvm mem attrs) into CSE branch
- **check**: Replace AnyVal placeholder in arrayAllocateFilled intrinsic with generic TypeVar
- **check/ir**: Audit and document all AnyVal inference fallbacks (WS-D2)
- Merge master into CSE branch pre-merge
- Merge perf/ir-redundant-read: escape-gated redundant-read CSE (Index/FieldGet) + LIN_NO_CSE gate + benchmark
- Merge branch 'refs/heads/master' into repr/single-form
- Merge branch 'refs/heads/master' into perf/tn
- Merge branch 'refs/heads/master' into perf/intunion
- Revert "perf(raptor): thread Frozen<T> through scanner — strided trip/stopTime reads (value-layout Stage 1)"

This reverts commit 495fd9227e9810a685c01b854a73d323b744c3f2.
- Revert "feat(types): add Type::Frozen<T> — immutable-proof wrapper for sealed-array strided reads"

This reverts commit c5b1b5c50eba952997c0577521bc6fbde58c49ef.
- Merge spike1-prep: perf(codegen) eliminate box/unbox cycle in sproj TAG_RECORD arm (−38% RAPTOR PREP)
- Merge branch 'master' into spike2-prep
- Merge spike2-prep: perf(ir) PATH-1 packed-view for some/every over sealed-record arrays (−4% PREP)
- Merge branch 'master' into spike3-prep
- Merge spike3-prep: perf(ir) fuse range(a,b,step).for(f) to native i32 counter loop
- Merge r2c-closdevirt: perf(ir) inline merge sort for sealed-record arrays — devirt comparator + kill projection cascade (−53% PREP)
- Merge r2a-projoffset: perf(codegen) const-offset GEP in TAG_RECORD projection arm (general projection win)
- Merge r3a-interp: perf(ir) lower string interpolation to single lin_string_build_n call (−6% PREP)
- Merge branch 'master' into bench/new-workloads
- Merge branch 'bench/new-workloads'
- Merge r3b-mapjoin: perf(ir) fuse xs.map(f).join(sep) into single-buffer build (−7% PREP)
- Merge r3c-sinkpure: perf(ir) sink pure single-branch-used val into its branch (general alloc-skip)
- Merge nbody-rc-elide: elide RC retain/release on read-only global array reads
- Merge nbody-sqrt: lower sqrt to llvm.sqrt.f64 intrinsic
- Merge nbody-borrow-args: pass non-escaping array args by borrow (no retain/release)
- Merge branch 'master' into spike-seal-a
- Merge branch 'spike-seal-a'
- Merge r4b-sealedalloc: perf(ir) close map-join packed-view gap for sealed-record element reads (general)
- **dijkstra**: Generate graph in-code, like-for-like across all languages
- Merge master into perf/nbody (incorporate seal/packed-view perf work)
- Merge perf/nbody: nbody 1319→358ms (3.7×) via RC-elide on global array reads, sqrt→llvm.sqrt intrinsic, and borrow non-escaping array args
- Merge branch 'master' into bench/dijkstra-like-for-like
- Merge branch 'bench/dijkstra-like-for-like'
- Merge branch 'master' into nbody-res-a
- Merge branch 'nbody-res-a'
- Merge branch 'master' into exercises-aoc
- Merge branch 'master' into nbody-res-b
- Merge branch 'nbody-res-b'
- Merge branch 'docs/fold-async-design'
- Merge branch 'docs/rewrite-memmgmt'
- Merge r6-interpjoin: perf(ir) fuse interpolation parts directly into map-join buffer (kill per-stop fragment allocs)
- Remove doc
- Merge branch 'docs/rationalise-adrs'
- Merge branch 'master' into posidx-a
- Merge branch 'posidx-a'
- Merge branch 'master' into ph3-a
- Merge branch 'ph3-a'
- Merge master into kn-getset (knucleotide substring + getset map fusion)
- Merge kn-getset: knucleotide 216→115ms (−47%) via substring→byte-slice map-key fusion + get-set upsert fusion

Two new IR passes (substr_map_fuse, getset_map_fuse) + runtime byte-keyed map ops
(lin_map_get_bytes pub, lin_map_set_bytes, lin_map_upsert_slot_bytes). A substring used
only as a map key skips heap allocation; a get-then-set on the same key lowers to one
probe. Three soundness fixes during review (value-use disqualification, call-in-window
dangling-slot guard) each locked with a regression test. RESULT=248211949 + RAPTOR digest
26203913/773022892/139 exact; valgrind clean.
- Tidy up
- Merge branch 'master' into perf/load-campaign
- Merge branch 'fix/sumnode-uaf'
- **raptor**: Delete the lin and lin-typed ports, keep lin-manually-typed
- Merge branch 'bench/raptor-prune'
- Merge branch 'fix/cross-module-parse-error-location'
- Clean up
- Merge branch 'master' into load-campaign-wt
- Merge branch 'master' into perf/raptor-flat-stoptimes

# Conflicts:
#	benchmarks/compare/raptor/lin-manually-typed/src/raptor/raptor-algorithm-factory.lin
#	benchmarks/compare/raptor/lin-manually-typed/src/raptor/raptor-algorithm.lin
#	benchmarks/compare/raptor/lin-manually-typed/src/raptor/raptor-algorithm.test.lin
#	benchmarks/compare/raptor/lin-manually-typed/src/raptor/route-scanner.lin
#	benchmarks/compare/raptor/lin-manually-typed/src/raptor/scan-results.test.lin
#	crates/lin-codegen/src/codegen/mod.rs
- Merge branch 'master' into perf/raptor-flat-stoptimes
- Merge branch 'master' into perf/raptor-flat-nodegoru
- Merge master into perf/loop-vectorize
- Merge commit '1cc3927b' into feat/cross-module-inline
- Revert "perf(ir): cross-module inlining of trivially-pure imported functions"

This reverts commit e22f6fb796c0c459dc31f2e6c5c429aae19af31a.
- Stage-1 flat stopTimes + bit-packed flags + stopIds rename (on current master 6fdcb6da)
- Merge branch 'master' into fix/tuple-in-union-coerce
- Merge branch 'master' into perf/raptor-drop-trip
- Merge branch 'master' into worktree-agent-a48c8c42966b2a2fb
- Merge branch 'master' into perf/raptor-drop-trip
- WIP raptor clean up
- Merge branch 'fix/sumnode-kind5-namespace'
- Merge branch 'master' into perf/group-scan-unbox
- Merge branch 'latest-master-tmp' into worktree-agent-a4a38ad2e8462715f
- Merge master into fix/partial-apply-generic

### Performance

- **runtime**: Fast hasher for LITERAL_CACHE (was SipHash)
- **codegen**: Emit string literals as constant LinString globals
- **codegen**: Keep records PACKED into mixed-union value slots (TAG_RECORD)
- Perf notes
- **ir**: Fuse range().for() into a native counter loop post-monomorphization
- **ir**: Recognize overloaded/monomorphized flat producers (no tagged reads)
- **codegen**: Mark user functions Internal linkage (INT E0.2)
- **codegen**: Add opt-in profile-guided optimization (LIN_PGO_GEN / LIN_PGO_USE)
- **codegen**: Inline RC retain/release ops; non-atomic hot path
- **ir**: Elide provably-redundant is-T / null checks (CK.2)
- **ir**: Devirtualize bare-fn combinator callbacks (path-8-B) [rebased]
- **ir**: Wave C lowerer — direct dispatch for find/some/every bare-fn predicates
- **ir**: Inline entries(map, f) over Type::Map via direct slot-walk loop
- **codegen**: Alwaysinline box/dispatch shims + hoist mid-body allocas to entry block
- **codegen**: CK.1 bounds-check elision + IRCE for range-for flat reads
- **ir**: Generalize CallbackDevirt to all callback combinators (CL.3)
- **codegen**: Inline lin_map_get first-probe fast path for string-keyed maps
- **ir**: Box/unbox cancellation peephole pass (RT.2b)
- **ir**: CL.4 LSS v1 — inline stored capturing-lambda callbacks at combinator sites
- **codegen**: VA.1 CPR — flat-union return ABI for scalar-or-null functions
- **codegen**: BL.1 bitcode-runtime merge pass (LIN_BC_RUNTIME=1)
- **ir**: Inline zero-arg while(() => Boolean) into a direct loop
- **runtime**: Cache FNV-1a hash in LinString header; codegen loads it directly
- **reccpr**: `is T` on T|Null NullableRecord uses ptr-null check
- **ir**: RC-ELIDE PATH-2 — suppress sealed-elem materialization for view-only block bindings
- **map**: SwissTable ctrl-byte layout for LinMap probe
- **csv**: Eliminate per-field from_utf8_lossy allocation in record_to_object
- **frozen**: Immortal early-out for retain_sealed_payload_fields
- **codegen**: Eliminate dead lin_map_get from sealed-array elem materialization
- **ir**: Extend SealedArrayFieldGet to heap fields in sealed arrays
- **csv**: Eliminate per-row round-trip and intern header keys in recordRows
- **codegen**: Const-offset TAG_RECORD reads in sealed_project_from (Stage 2)
- **runtime**: Lazy nested sealed reads (TAG_RECORD) — stop materializing records into maps (Stage 3)
- **runtime**: 0xFE element read -> TAG_RECORD + DELETE materialize_*_to_map family (Stage 4c) — record->map conversion no longer exists
- **csv**: Column projection in recordRows — skip unused columns
- **ir**: Redundant-read elimination for Index/FieldGet (escape-gated CSE)
- **stdlib**: Three stdlib perf/correctness fixes (unique scalar, sliding O(w), at codepoint)
- **codegen**: Structural LLVM memory-effect attributes for runtime functions
- **ir**: Harden redundant-read CSE pass + add LIN_NO_CSE gate + benchmark
- **runtime**: Intern CSV field strings for pointer-identity map keys (spike)
- **rc-elide**: Elide CloneBox/Release pairs for read-only union map-get results
- **clos2**: Devirt heap-accumulator reduce — inline capturing-lambda reduce with Map/Object acc
- **intunion**: Pure-IntLit-union sealed scalar — DayOfWeek seals RouteScanner
- **codegen**: Inline 0xFD pointer-spine load in sealed_array_materialize_elem
- **raptor**: Thread Frozen<T> through scanner — strided trip/stopTime reads (value-layout Stage 1)
- **ir**: PATH-1 packed-view for some/every over sealed-record arrays
- **codegen**: Eliminate box/unbox cycle in sproj TAG_RECORD arm — −42% RAPTOR PREP
- **ir**: Fuse range(a,b,step).for(f) to a native i32 counter loop
- Perf(ir) devirt sort comparator for sealed-record arrays via inline merge sort

Extend try_inline_scalar_sort (monomorphize) and lower_sort (combinator)
to fire for sealed-record (struct-pointer) element arrays, not just flat
numeric scalars. When sort(arr, cmp) is called with a capture-less literal
comparator over a named/sealed record array, the comparator body is now
spliced inline into the merge comparison site with no closure alloc,
no __cls_wrapb_ wrapper, and no per-comparison element boxing.

For the sealed-record path, lower_sort allocates two sealed-ptr buffers
via ArrayAllocateFilled (seeded from arr[0]), copies elements in via
IndexSet (retain-before-release, RC-safe), and merges with the inline
comparator accessing sealed struct fields as direct GEP loads.

RAPTOR PREP: -53% (2702→1276 ms/iter). The win is larger than the
closure-overhead fraction alone because the old generic std_array_sort
also boxed/unboxed each Trip pointer through the tagged-Val ABI per
comparison; the inline path eliminates that entirely.

DIGEST: group=26203913 range=773022892 journeys=139 (exact)
- Perf(codegen) const-offset GEP in TAG_RECORD projection arm (−16% PREP)

Replace lin_record_read_i64/lin_record_read_ptr_retain descriptor-walk
helpers with sealed_field_get const-offset GEP in the TAG_RECORD arm
of sealed_project_from, when the source record layout is statically known.

Adds sealed_project_from_hint(src, src_ty, target_fields, src_fields_hint)
that accepts an optional source-fields map. When Some, the TAG_RECORD arm
uses sealed_field_get (const GEP) instead of the by-name descriptor walk.
The hint is sound iff every target field exists in src_fields — verified
at codegen time via the `use_offset_path` guard.

Updated call sites that know source layout == target layout:
- boxing.rs: unbox_value + unbox_tagged_val_to_type — sealed→same-sealed
  roundtrip through the stdlib combinator boxed ABI; source IS target type.
- array.rs: sealed_array_rebuild_from_boxed — rebuilding arr_ty elem into
  itself; source fields == target fields.
- match.rs: sealed Coerce and NullableRecord projection — type safety
  guarantees source TAG_RECORD has same layout as target.
- index.rs: NullableRecord map-get hit — same type safety argument.

IR before: 282 lin_record_read_i64/ptr_retain calls in PREP IR.
IR after:  0 actual calls (only `declare` stubs remain).

PREP harness: 2684 ms/iter → 2245 ms/iter (−16%, min-3 low-load).
- **ir**: Lower string interpolation to single lin_string_build_n call
- **ir**: Fuse xs.map(f).join(sep) into single-buffer build
- Perf(ir) sink pure single-branch-used val into its branch (R3-C spike)

Adds a new TypedAST-level optimization pass `sink_pure_val` that moves
`val x = <pure expr>` bindings that are used only in one branch of a
subsequent `if` into that branch, so the expression is not computed on
the other path.

Motivating case: RAPTOR's `sortedTrips.for` callback computes
`val path = trip["stopTimes"].map(s => s["stop"])` on every trip but
uses `path` only in the `if routeStopIndex[routeId] == null then` branch
(the "new route" case, a small minority). After sinking, the map loop
and array allocation only execute when a new route is first seen.

Implementation:
- Pre-lowering TypedAST transform; RC correctness comes free because the
  lowerer emits alloc+RC fresh for the sunk expression in the branch.
- Two patterns: (1) Val trailing a block whose expr is If, (2) Val
  anywhere before an Expr(If{...}) stmt in the same block — non-contiguous
  scan, requires no mutation in intervening stmts and no use of the slot
  in stmts after the if.
- Purity gate: reads, arithmetic, pure combinator calls (map/filter over
  pure callbacks), constructors. Any unknown side-effecting call, IndexSet,
  LocalSet = impure.
- LIN_NO_SINK=1 to disable (same convention as other passes).
- Applied to both main-module and imported-module lowering paths so it
  fires inside imported for-callback bodies.

Measurements: digest EXACT (group=26203913 range=773022892 journeys=139),
ASan-clean, cargo test 0 failed (1 unrelated UDP socket test is
pre-existing flaky). PREP improvement is within noise (~1-3%) because
LLVM O2 already partially hoists the work; the pass is correct and fires
as confirmed by IR inspection.
- **ir**: Elide RC retain/release on read-only global array reads
- **codegen**: Lower sqrt to llvm.sqrt.f64 intrinsic
- **ir**: Pass non-escaping array args by borrow (no retain/release)
- **ir**: Elide bounds checks on proven-inbounds flat-scalar-array reads
- **ir**: Compile self-recursive named record types as packed sealed structs
- **ir**: Packed-view fast path for sealed-elem map.join (R4-B)
- **ir**: Fuse substring map-keys to byte-slice ops (skip per-lookup alloc)
- **ir**: Fuse get-then-set on same map key into one upsert probe
- **ir**: TBAA + noreturn on flat-array OOB paths for LICM hoisting
- **ir**: Fuse map().join() interp body directly into strbuf (R6)
- **ir**: Make [] positive-only for flat-scalar arrays, drop negative-index wrap
- **ir**: Teach bounds_elide to track arrayAllocateFilled lengths
- **raptor-bench**: Freeze load-once trips+transfers before create() — PREP ~1700→~960ms
- **ir**: Cross-module inlining of trivially-pure imported functions
- **runtime**: Mimalloc default allocator — RAPTOR LOAD 3741→2170 ms (−42%)
- **runtime**: Drop per-field intern stat thread_locals from CSV hot path
- **runtime**: Borrow CSV intern table once per row, not per field
- **runtime**: Pool Vec<u8> field buffers in CsvAssembler — ~14M alloc+free/load → reuse
- **runtime**: Bulk-scan unquoted CSV bytes with extend_from_slice
- **codegen**: Vectorize flat-scalar array loops via IndexSet bounds elision
- **raptor**: Full integer/global-array (paper) representation in node/go/rust ports
- **raptor**: Integer/global-array (paper) data model in lin-manually-typed port
- **ir**: Inline while/loop bodies that capture a packed-array-element view
- **ir**: Pair-elide — skip [k,v] pair-tuple alloc in entries() destructure callbacks
- **runtime**: Dense-array representation for integer-keyed maps
- **codegen**: Direct GEP for sealed { IntLitUnion: V } indexing
- **ir**: Stack-allocate non-escaping var cells (entry-block alloca)
- **codegen**: Alwaysinline small leaf user functions
- **codegen**: Stack-promote loop-captured non-escaping scalar var cells
- **raptor**: Dense-array interchange/scanPosition/queue + inline stopTimeIndexOf
- **raptor**: StopRoutePos parallel array — drop routeStopIndex map from the scan
- **raptor**: Hoist loop-invariant timetable reads out of getTrip/scanRoutes/scanTransfers
- **ir**: Elide RC on read-only borrowed FieldGet/Index collection results
- **raptor**: ScanRoutes tripIdx Int32 sentinel — drop union boxing in the scan loop
- **raptor**: RunsOn bind dates lookup once — halve map probes on the exception path
- **raptor**: Hoist prevArrivals/bestArrivals rows out of the scan loops

### Refactor

- **calc**: Literal discriminants, typed Step/Parsed, drop isFailure
- **report,processes**: ?? idiom, point-free maps, .map for TaskResult[]
- **raspberry-controller,event-transfers**: Combinators, typed arrays, stale comments
- **web-server**: Literal discriminants, ?? idiom, data-flow Dijkstra helpers
- **docs-site**: Is Error narrowing, typed handler, dirName→dirname, extractTitle→find
- **examples**: Fully type calc evaluator + motor literal-union channel
- **examples**: Tidy stale 'not supported' workarounds (unblocked by compiler fixes)
- **runtime**: Record_walk_fields/box_field_value primitive + descriptor-walk string.rs views (Stage 4a) — no intermediate LinMap for toString/json/keys
- **runtime**: Descriptor-walk equality + dynamic_to_map (Stage 4b-E1) — no intermediate LinMap
- **runtime**: Descriptor-walk array display/eq (Stage 4b-E2) — no intermediate LinMap
- **runtime**: Descriptor-walk IO/misc consumers (Stage 4b-E3) — no intermediate LinMap
- **raptor**: Consolidate timetable into a single Timetable struct + tidy-ups
- **raptor**: Name the id/offset types consistently in the timetable
- **raptor**: Drop redundant Int32 annotations + unnecessary toInt32/toUInt32 in the scan
- **raptor**: Drop the editorializing range/Int32 comment
- **raptor**: Drop dead gStopTimes binding + the g- prefix in scanRoutes
- **raptor**: Inline tt["routes"]/tt["routeStops"] (drop the hoisting locals)
- **raptor**: Inline the routes/stopTimes/trips array aliases in getTrip
- **raptor**: Inline the route field reads in getTrip
- **raptor**: Inline boardingPointIndex + drop tripRec in scanRoutes
- **raptor**: Drop dead stopRoutes/stopRoutesBase from Timetable (11 -> 9 fields)
- **raptor**: Tidy the algorithm factory
- **raptor**: Extract the pass-1 loop into named helpers
- **raptor**: Drop Trip object from timetable; (routeId,tripIndex) connections + flat services/tripIds
- **raptor**: Connection back to a tuple now the tuple-in-union compiler bug is fixed
- **raptor**: Production cleanup — semantic Connection aliases + flag helpers
- **raptor**: StopTimeIndexOf + (array,index) time/flag helpers; semantic Connection types
- **raptor**: Group timetable into routes/trips/stops; dense routeStopIndex
- **raptor**: Intern footpath transfers to int endpoints at build time
## [1.0.0] - 2026-06-21

### Bug Fixes

- Correct publisher ID to LinusN in package.json and release notes
- Fix stdlib tests
- Use block-form then/else in groupBy and countBy
- Retain/release balance for Function-typed params in stdlib calls
- Always rebuild mathlib.a for current platform in FFI test
- **stdlib**: Revert object.lin closures to lin_for
- Unbox TaggedVal* before using as array size in lin_array_allocate*
- Extend expr_is_owned_alloc to If/Match/Block and update phase 2 docs
- Bundle liblin_runtime.a with release binary, add unused import warnings
- **lin-ir**: Reach full IR-path parity — array/projection RC, curried partial app, rc_elide escapes
- **release**: Explicitly build lin-runtime staticlib before packaging
- **lin-ir**: More IR-import RC corners — 13/14 stdlib
- **lin-ir**: Complete IR-compiled imports — 14/14 stdlib, 128/128 integration, ASan-clean
- **lin-ir**: Top-level var accumulation in loop closures + boxed arithmetic operands
- **codegen**: Unbox boxed union operands for bitwise/shift ops
- **lin-ir**: More IR-import RC corners — 13/14 stdlib
- **lin-ir**: Complete IR-compiled imports — 14/14 stdlib, 128/128 integration, ASan-clean
- **codegen**: Release old value on concrete-rc var/global reassignment
- **codegen**: Drop Iterator from ty_is_concrete_rc to match lowerer is_rc_type
- **codegen**: Panic on undefined SSA temp instead of silent miscompile
- **runtime**: Write_lines wild-pointer read + flat-array free layout
- **rc**: Symmetric owning RC model for union (Json) var-cells/globals
- **rc**: Re-apply union-cell RC codegen onto split codegen module tree
- **check**: Restore §26 integer-literal narrowing in infer_call
- Boxed unsigned integers display/compare as unsigned
- Flat UInt32[]/UInt64[] arrays display as unsigned
- **ir**: Release discarded for-callback boxed return per iteration
- **codegen**: Sign-extend signed integers on widening coercion
- **lin-ir**: Reclaim per-iteration for/while element box (shell), fixing ~36 B/iter leak
- Box arguments to Json params in indirect (closure-value) calls
- **ir**: Free caller-owned arg-box shell after concrete-heap→union call
- **ir**: Free transient coercion box shell on LocalSet store into Json cell/global
- **runtime**: Structural deep equality for heap/nested array elements
- **lin-ir**: RC-elision double-counted releases (use-after-free)
- **lin-ir**: Match `is <binding>` arms — match unconditionally + unbox the binding
- **lin-ir**: Top-level non-function val referenced in an imported function
- **stdlib**: Concat preserves flat element type (UInt8[] etc.)
- **codegen**: Widen Float32 to Float64 (and narrow back) across all contexts
- **ir**: Transfer fresh heap-literal ownership into escaping call results
- **runtime**: Lin_array_eq heap-overflow on flat scalar arrays
- **codegen**: Uniform boxed ABI for function values (named fns, closures, partials)
- **ir**: Clone a borrowed union projection escaping as a function result
- **ir**: Materialize a top-level named fn referenced as a value (Function arg)
- **ir**: Short-circuit && and || (spec §24)
- **ir**: Own each if/match branch result; transfer block survivors to parent scope
- **lex**: Line-leading .method is a dot-chain continuation (no INDENT/DEDENT)
- **ir**: Free provably-non-escaping captured `var` cells at scope exit
- **codegen**: Object/array index-assign of a boxed-Json callback value/key
- **codegen**: Order-symmetric equality/comparison for boxed-union operands
- **ir**: Unbox boxed-Json operands for bitwise ops (& | ^ << >>)
- **runtime**: Null-safe lin_string_eq (String == null must not crash)
- **json**: Close fromJson cast-hole headline case + make is Error discriminate
- **match**: Is <ObjectType> checks required fields in expression form (ADR-050)
- **check**: Bind type params when resolving generic alias bodies
- **lex**: Treat '-' after '[' as a negative literal, not subtraction
- **closures**: Own captured values (retain on capture, release on free) — ADR-051
- **parse**: Postfix nested array types T[][] (Array of Array)
- **parse**: Multi-line tagged-union type bodies (spec §18 form)
- **check**: Push declared object/union type into match/if arms
- **check**: Honour numeric literal suffixes; large bare literals widen, not truncate
- **codegen**: ArrayAllocateFilled now fills slots (was returning all-null)
- **vscode**: Set extension icon in package.json
- **vscode**: Add extension README for marketplace Overview
- **runtime**: Concat retains copied elements — fix use-after-free on fresh values (ADR-064)
- **runtime**: Tag split() elements TAG_STR so for/map see strings
- **docs**: Prefix links/assets with LIN_DOCS_BASE for project Pages subpath
- **codegen**: Dispatch boxed-operand arithmetic on runtime tag, not Int32
- **generics**: Bind T=Json for Json args; safe import monomorphization of unbound TypeVars (Phase 6-pre)
- **lin-ir**: Retain borrowed element on filter's tagged object-array push (ADR-069 R2)
- Correct stale ADR-070 -> ADR-065 in await test comment
- **ir**: Bound combinator loops with tag-checked length for non-array Json (ADR-068 follow-up)
- **runtime**: Lin_tagged_to_string must return an owned string (UAF/double-free)
- **compile**: Detect circular imports at compile time instead of stack-overflowing
- **check**: Clean up union-if type shape surfaced by adversarial review
- **cli**: Lin check resolves imports (was silently passing import-dependent type errors)
- **fmt**: Never emit paren-less lambda outside arg position (round-trip safety)
- **runtime**: Parallel() handles already-spawned promises, not just thunks
- **check,codegen**: Two array-allocation/iterator bugs surfaced by the dijkstra benchmark
- **streams**: Re-key affine consume-check off dispatch (close linesMax/promise/close UAF holes)
- **docs-site**: Honor backslash-escaped pipes (\|) in table cells
- **parser**: Column-delimited if/else branch blocks inside parens
- **parser**: Column-delimited match arms inside parens
- **ir**: Widen int elements to float in mixed flat array literals
- **lsp**: Derive stdlib completions from file imports (no hardcoded list)
- **test**: Record test output in VSCode Test Results (appendOutput + forward user print)
- **compile**: Resolve coverage profile runtime via clang driver; surface linker stderr
- **fmt**: Preserve block-bodied bindings and stop duplicating branch comments
- **check**: Support mixed numeric families in one Number call (bug #2)
- **check**: Accept Json at a Number param (direct + projected), consistent
- **check**: Clear error for Number in binding position (not a value type)
- **lsp**: Cycle guard in pre_resolve_imports + sync stdlib list
- **runtime**: Panic-safety at FFI boundaries (broken pipe, lock poisoning)
- **parse**: No-progress guard against parser hangs + formatter preserves partial-application comma
- **lin-ir**: Sound RC elision — post-dominance for cross-block, liveness gate, intervening-retain interference
- **codegen**: Share runtime tags via lin-common; fix Float32/64 tag + UInt64 signedness
- **compile**: Import-signature-aware cache key + on-disk version stamp
- **examples**: Codec uses push-coercion for length prefix instead of signed Int32 -> UInt64 cast
- **runtime**: Pass trailing index arg to stream combinator callbacks (macOS/arm64 fault)
- **fmt**: Canonicalize docs-site/builder/markdown.lin (CI fmt --check drift)
- **lsp**: Add std/ffi to stdlib_source list (sync with compiler; fixes stdlib_modules_match_compiler)
- **lsp**: Register std/ffi in stdlib_source
- **ir**: Materialize sealed scalar records at spread / array-elem / object-field-value boundaries
- **ir**: Closure-local var reassigned in if-branch must persist past the join
- **ir**: Imported-module top-level var mutated by an exported function
- **check**: Mixed Int32*Int64 widens the Int32 operand, not the result
- **runtime**: Dynamic Json arithmetic faults on non-numeric operand (#5) + hashed-object proposal (#4b)
- **parse**: Actionable ';' diagnostic + regression test for CLI args
- **parse**: Anchor if/else branch offside on the line, not the keyword column
- **codegen**: Nested { String: T } maps — dispatch TAG_MAP on union index read/write
- **monomorphize**: Substitute TypeVar in is/match-arm pattern targets
- **check**: Empty array literal arg adopts flat-scalar param element type
- **check**: Empty object literal arg adopts Map param value type
- **raptor-bench**: Annotate evidence-free empty literals as Json
- **check**: Reject Json -> { String: T } map coercion even in trusted stdlib
- **benchmarks**: Narrow await's T|Error before arithmetic in thread_pool/async_await
- **sealed**: Route scalar-record array push to contiguous layout (heap-corruption fix)
- **ir**: Projection loads + owns value (Stage A: close UAF)
- **install**: Resolve newest stable release by default
- **check**: Widen record literal to { String: T } in nested field position
- **ir**: Release Iterator-typed values (range/combinator results leaked)
- **sealed**: Correct generic combinators over sealed-record arrays (Problem A)
- **check**: Back-infer resolved generic T into unannotated callback params
- **codegen**: Unbox Type::Map in closure-ABI wrapper (was passing TAG_MAP box through as LinMap*)
- **sealed**: Correct generic combinators over sealed-record arrays
- **ir**: Free captured-var cells for std/iter combinators (stale module gate)
- **check**: Close record field-omission soundness hole in generic calls
- **codegen**: Sealed out-of-shape field access -> Null, not panic
- **rc**: Eliminate string-interpolation transient-use leak (leak #3)
- **check,ir**: Route Json-operand arithmetic through null-safe tagged path (was unboxing missing-key Null -> crash)
- **ir**: Reclaim box shell of fresh heap value bound to Json (leak B)
- **codegen,ir**: Reclaim tagged arith/cmp/eq operand box shells (RAPTOR leak #4b)
- **ir**: Release union-typed Binary (tagged-arith) result box
- **ir**: Release fresh combinator iterable consumed by for/while in closure body (RAPTOR leak #5)
- **runtime**: Release TAG_MAP values when an object/record is freed
- **codegen**: Release prior owned param-slot value on TCO back-edge
- **check,ir**: Fix packed-array producer/consumer literal drift + extend repr::verify to all repr-consuming opcodes
- **ir**: Release tail-call body-scope temps + fix array-index fresh-box double-clone (RAPTOR scanBack ~227MB/scan leak)
- **runtime**: Retain closures stored into object fields (escaping-capture UAF)
- **runtime,codegen**: Worker takes owning ref to handler/onClose closures
- **codegen**: Free arg-box shell when call result is a non-pointer (L2)
- **ir**: Reclaim caller-owned arg-box shells; drop obsolete escape-alias (L2, L4)
- **ir**: Reclaim map/filter per-element box (L3)
- **check**: Roll back leaked nesting state on discarded speculative callback check
- **ir,codegen,runtime**: Plug boxed sealed-record-array heap-field RC leaks
- **ir**: Release-free sealed-record literal in return position
- **ir**: Release pass-through-param read-retains on TCO back-edge
- **codegen,runtime**: Json/Object value used as a typed map { String: T } hangs/corrupts
- **runtime**: Thread failed-source Error in-band through stream ops
- **ir**: Mangle Type::Object by shape to stop monomorph symbol collision
- **codegen**: Box worker/shared transfer args on static type, not is_pointer_value
- **fmt**: Emit generic type application as Name<Args>, not Name[Args]
- **ir**: Monomorphize generic union-typed args by walking into union members
- **check**: Pin generic result type-param inside record fields & union index
- **runtime**: Async thunk capturing a function value runs on a worker, not inline
- **check**: Expand Named-alias return types so mutual-recursion record returns agree on repr
- **ir,check**: Erase phantom return-only generic params instead of erroring
- **ir**: Plug sealed-record-union (Trip|Null) leaks — materialization full-release, fresh-tail-param per-iteration release, match-narrow projection
- **coverage**: Attribute monomorphized-generic coverage to the generic's source
- **ir,codegen**: Scalar Float32 return fpext + flat-array element-width coercion
- **check**: Adopt float literal at Float32 context type
- **ir**: Release per-element box in inline scalar-sort copy loop
- **ir,check**: Tail-return recursive sum literal pushdown
- **test**: Bound macOS TCP loopback accept flake with a retry
- **net**: Force accepted TCP stream to blocking mode (macOS portability)
- **vscode**: Harden lin problem matcher for ANSI codes and Windows paths
- **codegen**: Release loop-owned TCO param slots on return (Leak B)
- **codegen**: Guard TCO loop-exit release against ALL entry params (fix permuted-buffer double-free)
- **stdlib**: Plug per-element leak in generic merge sort's input copy
- **ir**: Release per-element sealed-record struct in map/for/while/reduce
- **codegen**: Release fresh-owned sealed-record-array field in sealed→Json materializer
- **parse**: Parenthesized type with postfix array suffix
- **check**: Push declared scalar width into integer/Float32-return bodies
- **codegen**: Packed-struct store for literal-key sealed-record field writes
- **codegen,ir**: Sum value materialize-to-boxed boundary correctness (unboxed-sumtype Stage 3)
- **ir**: Close untyped-object sum-store soundness hole (Stage 3)
- **codegen**: Release TCO param old-value by repr, not static type
- **ir**: No double-release of map/sum-projected arg (keep-packed {String:Expr} round-trip UAF)
- **codegen**: Release TCO loop-EXIT sum param slot by repr, not static type
- **lsp**: Use UTF-16 code units for LSP positions
- **lsp**: Make cross-file rename sound (no comment/string/shadow over-match)
- **lsp**: Poison-tolerant locks + panic-safe span slicing
- **lsp**: Update expr_binds_name to current AST arities after rebase
- **lsp**: Correct signature-help active param over comparison/lambda/string args
- **lsp**: Include polymorphic combinators in dot-completion
- **lsp**: Suppress completion inside strings and comments
- **vscode**: Harden test discovery, string escapes, and terminal quoting
- **vscode**: Make match snippet default a valid Lin record-pattern arm
- **vscode**: Soft CodeLLDB dep, F5 build timeout/identity-match, explicit VSIX packaging
- **check**: Narrow true-branch of `if x is X` to matched member for non-Json unions
- **check**: If-merge of a Json branch + a concrete branch must stay Json
- **lin-ir**: Widen sub-Int32 flat element into a branch PHI of wider int type
- **runtime**: ADR-063 — make 0xFE packed-record-array thread transfer sound (clone_sealed_array)
- **ir**: Release packed sealed array + orphaned unbox box on Json-view read
- **ir**: Route push/set into a container-stored array through the boxed path
- **codegen**: Skip width-subtyping extra fields in sealed_construct
- **ir,runtime**: Reclaim per-element box inner in for/while/reduce over heap-bearing arrays
- **codegen**: Project nested sealed-record-array field into packed buffer on push
- **ir,repr**: Classify nested sealed-record-array field reads as Packed
- **lsp**: Treat span offsets as char offsets in position conversions
- **repr**: Narrow sealed-array packability gate back to scalar+Bool (Stage 3a)
- **check**: Count type exports; fix latent bad imports CI surfaced
- **compile**: Make unclassified link-failure message honest
- **vscode**: Drop bare-prefix combinator snippets that hijack LSP dot-completion
- **lsp**: Render generic type params instead of raw inference vars
- **codegen**: Retain union element pushed into concrete heap T[]; make iter.flatMap generic
- **stdlib/object**: Type keys/values/entries param, closing the scalar hole
- **ir**: Two RC-soundness fixes for concrete values crossing the union boundary
- **ir**: Boxed-fallback cross-module generic that reads an origin global var; make random.pick/shuffled generic
- **check**: Flow-narrow across && and unbreak RAPTOR under tightened keys/values
- **lsp**: No completion in val/var binding-name position
- **lsp**: Only offer exported (non-underscore) stdlib symbols in completion
- **lsp**: Keep trailing space when merging an auto-import into an existing brace list
- **ir**: Coerce boxed combinator-result array bound to packed sealed-array annotation
- **parse**: Clear error for a val/var binding with no name
- **check**: Accurate error + hint for scalar annotation on an array literal
- **ir**: Own the inner of a caller-owned-shell box arg threaded into a self-tail-call (Trip|Null UAF)
- **ir**: Release the per-iteration index/element box in non-inline for loops
- **ir**: Gate path-6 fusion to inline-scalar element flow (bail sealed/heap sources to per-stage path)
- Make iter.for generic over its element type (typed callback)
- **ir**: Coerce numeric width when a combinator callback widens the element
- **runtime**: Tagged array write sinks blind-wrote into packed sealed (0xFE) buffers — pack via the named descriptor instead
- **parse**: Parenthesized function type in return position
- **check**: If-merge over an opaque/Json function-call result is no longer mistyped onto the other branch
- **examples**: Handle udpBind Int32|Error in raspberry-controller runController
- **compile**: Link-error details no longer truncated to only ld warnings
- **compile**: Keep indented symbol-name lines in link-error details
- **compile**: Link CoreFoundation + IOKit frameworks on macOS
- **check**: Only seal a directed record literal when its fields are gate-packable
- **ir**: Reclaim map-produced/source values in multi-stage fused chains
- **codegen**: Release TCO param-slot for heap-bearing sealed records
- **monomorphize**: Make symbol name agree with dedup key about `sealed` (F1)
- **lower**: Release orphaned box on scalar-unbox tail-call arg (leg1 leak)
- **codegen**: Unbox boxed Json to Int8/UInt8/Int16/UInt16 (inline narrow-int path)
- **codegen**: Retain borrowed heap field in sealed_construct (Trip[] UAF)
- **lower**: Skip container-insert retain on sealed->boxed array element materialize (Trip[] leak)
- **lower**: Free freshly-boxed element shells in MakeArray over Json/union array (Json[] shell leak)
- **lower**: Release sealed-record source on widen-to-union tail-call arg (Trip|Null threading leak)
- **ir**: Add TarEntry to is_union_ty/is_union_owning_ty; fix header-object leak
- **check**: Gate async thunk against TarEntry captures at user call site
- **fmt**: Keep required parens around `??` operands of tighter operators
- **ir**: Audit tar-entries intrinsics into the ownership convention table
- **bench**: Point benchTarLoad harnesses at the main checkout paths
- **test**: Resolve git conflict markers left in integration.rs by the feat/tar-loader-typed merge (33b29a8d)
- **check**: Flow-narrow index places (m[k]) through if null-tests
- **codegen**: Bare sealed records as map values — materialize boxed, never keep-packed
- **ir**: Skip transfer_into_container for push into pointer-backed sealed array
- **repr**: Seed Index dst as PackedSealedArray when result_ty is a sealed array
- **ir**: D3a closure-captured param + inferred-literal arg garbage read
- **ir**: Generic inner-function LLVM symbol collision
- **codegen**: Stage-2a UAF fixes for 0xFD sealed-record-array heap fields
- **ir**: Free per-iteration var-cells in inline loop bodies
- **ir**: Don't field-strip a record coerced into the open {} type
- **runtime**: Migrate all cold TAG_OBJECT consumers to accept TAG_RECORD (Stage 6a leg-2)
- **reset/6b-del**: Regex compile — reconstruct Error so is Error narrowing sees TAG_OBJECT
- **fs**: Remove unreachable match arm in lin_fs_write_file_bytes
- **diagnostics**: Render module-not-found errors with ariadne source spans
- **diagnostics**: Attribute import type errors to the correct source file
- **runtime**: LinMap insertion-order keys + spread/rest insertion-order iteration
- **runtime**: Tagged_to_json TAG_MAP arm: iterate in insertion order
- **check**: Clear tail-position flag for statements in bidirectional block check
- **check**: Infer ?? default against the stripped left type
- **phase3-c1**: Migrate sealed-record materializers to LinMap (TAG_MAP)
- **phase3-c2**: Transfer TAG_OBJECT values as TAG_MAP (clone_object_as_map)
- **phase3**: Code-review fixes — UAF, data-loss, is-type, server; ordering=declaration
- **reset**: Code-review fixes on object.rs deletion
- **runtime**: Map values/entries use insertion order; tune alloc + load factor
- **runtime**: Memoize heap desc + intern field-name strings in sealed materialize
- **codegen**: Harden box/unbox catch-alls against unhandled Type variants
- **runtime**: Close 3 review-found UAF/correctness gaps (A5)
- **sealed**: NKIND_FLOAT32 separate from NKIND_FLOAT64 for correct 4-byte field layout
- **lsp**: Resolve imported type aliases in annotations
- **codegen**: Tag-dispatch union FieldGet (TAG_RECORD/OBJECT/MAP)
- **vscode**: Colour `type` keyword in `export type` declarations
- **codegen**: 8-byte-align nested pointers in sealed named descriptors (macOS ld64)
- **ir**: Remove unsound container-escape allowance from 0xFE inline gate
- **ir**: Drain 3 interp-benchmark leaks (33 MB/run → 668 B residual)
- **check**: Preserve all-scalar stack-residence — gate forward-declare patch on !is_all_scalar_sealed_record
- **check**: Preserve expected field order in sealed object literals
- Check array literals against expected tuple type in return position
- **check**: Attach leaf span to unknown-type diagnostics instead of enclosing decl span
- **check,parse**: Explain bare-key record types in diagnostics
- Correct sealed-record field offsets in materialize-to-map (toBe crash)
- Preserve int key-kind for maps nested as map values
- Dispatch integer-literal-union-keyed sealed records to correct string lookup
- **check**: Only group function exports into overload sets in ModuleSignature
- **lsp**: Preserve named record type alias in Type::Object for Display/LSP
- **vscode**: FindOrCreateTestItem must search suite groups, not just file's direct children
- Check array literals against expected tuple type in return position
- Narrow-int sealed NKIND codes + Union-keyed-map compound index routing
- **tags**: Correct stale NKIND_INT32 comment (Int8/Int16 now have own codes 15/16)
- **check**: Reject Function against AnyVal wildcard; tighten stdlib signatures
- **check,runtime**: Keys/values/entries accept any-keyed map; stringify int keys (ADR-086)
- **check**: Chained nested-map index resolves inner value type, not a fresh TypeVar (ADR-087)
- **compile**: Seed exported type aliases across cyclic import SCCs
- **check,codegen,ir**: Forward-declare local recursive function-vals with union return types
- **check**: Expand forward-referenced sibling types in exported aliases
- **check**: Restore module scope after a failed top-level statement
- **parse**: Delimiter-aware top-level error recovery (ADR-080)
- **ir**: Project fresh unsealed record into NullableRecord TCO arg, not boxed map
- **codegen**: Retain/release packed record at union static type by-rc, not by-tag
- **compile**: Resolve cross-cycle map alias param type via fixpoint type-alias seeding
- **check**: Resolve imported key alias of a map type defined in an import cycle
- **check**: Check object literals against substituted union param type in generic calls
- **stdlib/iter**: 3-arg range now yields Int32[] instead of AnyVal Iterator
- **check**: Infer empty-literal init in reduce/generic calls from expected return type
- **check**: Narrowed var binding retains mutability + invalidates after assign
- **check**: || right operand narrows into function-call arguments
- **check**: Merge same-name imports from different modules into overload set
- **check**: Computed key { [expr]: v } now infers Map type and inserts at runtime
- **check/codegen**: Solve forward-declared return TypeVar when function body is checked
- **codegen**: Allocate 0xFD sealed-ptr array for Intrinsic::ArrayAlloc on sealed element types
- **codegen**: Emit $defaultN wrapper for imported fn with T|Null=null default
- **ir**: Topo-sort inner fn stmts in block lowering to fix RC crash
- **runtime**: Pack NKIND_SEALED nested sealed-record fields from boxed elements
- **runtime**: Handle 0xFF sealed arrays in sealed_array_materialize_elem and sealed_any_to_tagged
- **codegen**: Keep sealed-record arrays packed when boxed into a union slot
- **codegen**: Keep sealed-record arrays packed when boxed into a union slot
- **check**: Don't box sealed:true non-packed records (Function-field) into Named params
- **check**: Don't record a forward-declared inner fn's own slot as a self-capture
- **raptor port**: Use this route's stop count for routePathLength, not the route map size
- **raptor port**: Append to kArrivals via push (Lin array index-set doesn't auto-grow)
- **parse**: Attach else to outer if by offside column inside parens
- **codegen**: Own the union->sealed-array return coercion (bug#2 journey corruption)
- **ir**: Don't full-release a sealed record passed to a Named param (UAF)
- **check**: Capture a closure referenced only via a dot-call
- **raptor port**: SearchDay day-cap was off by one (searched maxSearchDays+1 days)
- **raptor port**: ScanRoutes must skip an unreached stop (previousArrival==0 guard)
- **raptor port**: Record calendar_dates exceptions (init inner map + numeric key)
- **ir**: Release concrete-rc arrays per-registration on a tail-call branch

### Documentation

- Add VS Code extension installation and features to README
- Always build before test to avoid stale lin binary
- Update all if examples to new then-on-condition-line syntax
- Bucket remaining IR-parity failures against later phases
- Ledger status checkpoint (103/128 on IR leg) + remaining-work breakdown
- Ledger checkpoint 2 (113/128) + root-caused remaining 15
- Ledger checkpoint 3 (114/128) — stopping point + prioritized next steps
- Ledger checkpoint 4 (124/128) + final remaining-work analysis
- Ledger checkpoint 5 (127/128 integration) + final analysis
- Ledger checkpoint 6 — 127/128, AST-IR callback ABI corner root-caused
- Note IR-path imports still use AST register_import (Milestone 2 blocker)
- Checkpoint 8 — IR-compiled imports (128/128 integration, 11/14 stdlib)
- Checkpoint 8 — IR-compiled imports (128/128 integration, 11/14 stdlib)
- Delete IR_PARITY_LEDGER.md (parity reached, milestone complete)
- Add Lin-built documentation site generator and content
- Mark ARCHITECTURE_CLEANUP Phases 0-4 complete
- **COMPILER**: Update for IR-only backend + codegen module tree
- Use idiomatic dot-syntax in std/test toString example
- Bash installer, JSON-pipeline hero snippet, Lin-Language/Lin rename
- **spec**: Correct is/has semantics for object types (ADR-054)
- Backfill ADRs 056-059 for shipped low-level decisions; tick stale TODOs
- TEST_UTILITIES.md — std/test gaps + mocking proposals from the examples pass
- Drop TEST_UTILITIES.md from the branch (findings shared separately)
- Lead README with feature-oriented positioning
- **stdlib**: Correct STDLIB.md to match the actual stdlib exports
- **site**: Refresh content for current master, rewrite builder as markdown→JSON→HTML
- **site**: Two-column hero with feature list + tabbed code examples
- Fix partial application syntax (trailing comma) and default params
- Add optional/default parameters section to Functions tutorial
- Add generics coverage (generic functions, variance, precedence)
- **site**: Add Threading and HTTP hero tabs
- **site**: Http hero tab — add GET request, pattern-match the server
- Add Generics and HTTP & Web tutorials
- **ADR-068**: Correct perf claim — neutral at -O2, not 1.64x (debug-build artifact)
- Add Arrays section to the Values & Types tutorial
- Fixed-length array types now work — drop the caveat
- Document await's T|Error enforcement (§32.2.2, ADR-070)
- **ADR**: Consolidate DECISIONS.md (70 headers -> 50) and repoint cross-references
- **ADR**: Repoint stray ADR-068 ref to ADR-069 (post-merge consolidation)
- Align fallible-stdlib error shape with canonical Error value
- Holistic spec rewrite (restructure + correct to match impl), remove stdlib index
- Remap spec §refs in TODO/ASYNC_DESIGN/MEMORY_MANAGEMENT/README to new numbering
- **examples**: Fix stale test-file names in calc/codec/config READMEs
- **streams**: Design brief for Stream<T> feature
- **streams**: Final status summary in STREAMS_PROGRESS.md
- **iter**: Record Stage 1 verification + length/push wart in tracker
- **iter**: Record Stage 2 verification + take double-impl reconciliation TODO
- **iter**: Stage 6 code-side verification (flat-producer + affine + final sweep)
- Update CLAUDE.md stdlib list + combinator home (std/iter, std/stream)
- **streams**: Add ADR-073 (affine resource types + move-transfer) and ADR-074 (Stream vs Iterator + for/terminal error semantics)
- **streams**: Add SPECIFICATION.md §27.9 Streams; repoint §27.9->§27.10 cross-refs
- **streams**: Document std/stream module in STDLIB.md (index + functions + reference)
- **iter**: ADR-075 unified iterable combinators via receiver dispatch (std/iter)
- **iter**: STDLIB std/iter module + revise array/stream sections
- **iter**: SPECIFICATION receiver-dispatched combinators (§18.7) + cross-refs
- **iter**: Fix std/bytes example to import for from std/iter
- **iter**: Fix headline-win example (drop import, avoid at-Null in callback)
- **iter**: Fix EXAMPLE.md combinator import (std/array -> std/iter)
- **site**: Add std/iter + std/stream pages, migrate combinator imports, nav
- Format multi-stage stream/combinator chains as multi-line (idiomatic style)
- **site**: Migrate combinator imports in homepage examples to std/iter
- Rewrite stream lifetime/promise/close prose for end users (drop fd/RC/affine jargon)
- **docs-site**: Add std/compress and std/archive pages + nav entries
- **ADR-078**: Reject the Rust->Lin stdlib migration on pilot evidence
- **imports**: Correct ADR-078 cyclic-import inference limitation + characterize gap
- **style**: Use dot-application in running-code examples
- **site**: Sync stdlib API docs with updated signatures
- Sealed records design proposal
- **raptor**: Correct LIN_ISSUES #7 — multi-line if/else unparseable inside parens
- **raptor**: LIN_ISSUES #7 is one parser bug, not a formatter bug
- **proposals**: Add agent implementation brief for O(1) Json key lookup (#4b)
- **proposals**: Typed map / index-signature type ({ String: T } / Map<K,V>)
- **proposals**: Lock in Option A (index-signature { String: T }) for typed maps
- **proposals**: Stdlib Json typing audit — 145 sigs classified into 5 fix categories
- **decisions**: Record hashed Json object side-index as ADR-081; retire implemented proposals
- Remove shipped Category 1 from Json audit; sync docs-site stdlib signatures
- ADR-082 sealed records + SPECIFICATION §5.9.1; retire design doc
- Retire typed-map-index-signature proposal (shipped as ADR-082)
- Renumber sealed-records ADR 082→083 (master landed its own ADR-082)
- **examples**: Inline match in map callbacks; drop obsolete ADR-004/014 workaround
- **ADR-069**: Correct stale R1 note — inline match in combinator parens is supported
- **audit**: Record generic-callback gap + Category 3 partial status
- Retire stdlib-json-typing-audit.md (Categories 1-3 shipped); fix stale readFile mock sig
- **stdlib**: Clarify why keys/values/entries stay Json (rule now enforced, not a workaround)
- **proposals**: Root-cause the projection-aliasing UAF (interior pointer dangles on container grow)
- **changelog**: Mirror release notes to docs-site
- Record release automation in ADR-087 and CLAUDE.md
- Rationalise and renumber ADRs contiguously (ADR-001..060)
- **ir**: Document the string-interpolation transient-use leak in lower_string_interp
- **raptor**: Refresh benchmark results after hashed-map migration
- Representation-inference pass design blueprint (working reference)
- Fold repr-pass design into ADR-062, add spec note, delete design doc (Part E)
- Unboxed sum-type design blueprint (working reference for the build)
- **event**: Document std/event in STDLIB.md and docs-site
- **tutorials**: Add Streaming I/O (13) and Events (14) tutorials
- Repoint testing/mocking docs from deleted examples/dijkstra to web-server; fix lin-engineer wordcount->report/frequency ref; rm empty codec dir
- Add stdlib expansion proposals + build brief
- **sumtype**: Record Stage 3+4 results + ADR-064 (keep-packed via TAG_SUMNODE, measured interp win)
- **lsp**: Document why extract_exports omits var/type exports (FIX 5 skipped)
- **vscode**: Note Phase 3 auto-renders Lin locals via pointer cascade
- **raptor**: Mark the typed-map-as-Json TAG_MAP index bug as FIXED (0819c17)
- Generate stdlib docs-site from .lin doc comments + migrate prose into source
- Remove implemented stdlib proposal specs + agent briefs
- **proposals**: Correct path-3 H8 — borrow-ABI prototype is UNSOUND, not 'sound/~1.5×'
- **proposals**: Drop userland-language-change options; the seal/hash distinction is already syntactic
- **stdlib**: Readability rewrite + type tar/stream surfaces; fence docs code; compact<T>
- **proposals**: Add path-4 whole-program region inference
- **proposals**: Record implementation findings for path-0/1/3
- **proposals**: Add branch/commit references to path-0/1/3 findings
- **proposals**: Record path-0/2/3 corroborating findings (from agent run)
- **proposals**: Add the exact packed-iteration microbench + repro recipe to path-1 findings
- **proposals**: Path-7 (tracing GC, demoted/bounded by ceiling test) + path-8 (make functions free: 4-tier call-cost demolition)
- **proposals**: Retrospective — "de-Json-ing" is two levers (dict→Map vs record→packed)
- **path-8**: Fold in Tier-1 bitcode spike result — Tier 1 alone buys <2% (consumer stays opaque); Tier 2 leads, Tier 1 is the finishing pass
- Type aliases resolving to String are valid map keys
- **path-8**: Reconcile with RAPTOR profile — Json reads = 631M linear scans (interp=calls, RAPTOR=repr; partial typing regresses 13%; fix is end-to-end packed)
- **site**: Add object-types section — sealed records, hashmap types, aliased keys
- **proposals**: RAPTOR profile findings — falsify path-5 premise, complete path-8 cost map, add path-9 (end-to-end packed records, a Path-1 continuation)
- Docs
- Correct §5.9.1 — heap-field sealed records now pack (String/Array/Map/nested)
- **proposals**: Execution plan for path-7 (GC, one measurement then revive/retire) + path-8 (call-cost tiers, post-inplace-fusion-merge gaps)
- **path-7**: CLOSED-NEGATIVE — no workload is alloc-bound (LIN_NO_RC ceiling = ~0% everywhere incl RAPTOR's 0.039 retention); GC retired, levers are path-8/path-9
- **stdlib**: Readability pass + fix generator HOF-signature truncation
- **path-8,path-9**: Verified findings + branch refs — Step 8.1 record fusion ~2.3x (perf/path8-step1-record-fusion); 9C seal-propagation fixes live 7/0 corruption (perf/path9c-seal-propagation); 9-A String packing blocker fixed, linear-scan->const-offset verified (perf/path9a-widen-on-9c); stale prereq-stack status corrected
- **path-8,path-9**: 2nd-line findings — Tier-3 devirt DEAD END; 9D end-to-end MEASURED regresses (map-value seam); 9C face-2-vs-seal-prop reconciled (use face-2, combination unsound)
- **proposals**: Add paths 10-14 — the foundations strategy
- **proposals**: Restructure paths 10-14 into two self-contained adventures
- **raptor**: Mark LIN_ISSUES #1-#4 FIXED (verified 2026-06-10), flag file as historical
- **raptor**: Drop stale LIN_ISSUES references in loader comments
- **path-9**: Agent-1 findings — packing thesis CLOSED-NEGATIVE (net PREP regression, fix-for-a-fix chain); 3 salvaged standalone wins (8.1 fusion ~2.3x, 9C corruption fix, map-value direct-index ~1.55x) with branch refs
- **path-9**: Agent-2 findings — completing-RAPTOR A/B ANSWERS agent-1's open question, CLOSED-NEGATIVE confirmed by 2nd independent line
- **path-9**: Agent-3 findings — orchestrated line confirms CLOSED-NEGATIVE (3rd independent agreement) + TCO pass-through seam
- **path-9**: CONCLUSION — CLOSED-NEGATIVE (3 lines agree ~1.8x slower); salvage merged, packing chain retired
- **rc**: Rewrite MEMORY_MANAGEMENT around the live LinIR pipeline
- Add PERFORMANCE.md — measured perf characteristics + path-n learnings
- Delete path-* perf proposals (distilled into PERFORMANCE.md §5)
- **adr**: Audit — annotate ADR-044/062/063 for this session's repr/inline changes
- **adr**: Add ADR-065 — ownership-as-a-fact, one combinator loop emitter, flatMap fusion, lambda-set devirt
- **perf**: Pin the typed-RAPTOR ~2x seam (Trip|Null union scan) + note leak-free
- **agent**: Teach lin-engineer the ?? operator + baked-in optional chaining (kill match-is-Null default chains)
- **agent**: Flag the calc example's Json as the anti-pattern, not the style to copy
- **archive**: Fix entries-close doc; add body().promise() warning and ADR note
- **adr**: ADR-067 — heap-field discriminated SumNodes (landed) + the T|Null repr frontier (sound path = tagged nullable-SumNode; raw-pointer retired)
- **perf**: Update typed-vs-Json RAPTOR with de-materialization results
- Design doc + staged plan — the representation reset (records as value types, dissolve LinObject, Json->Any)
- Split representation vs semantics axes; choose reference semantics
- Pin Stage-0 decisions (D1-D8) from design review
- Implement-then-document ordering (spec follows the code, not before)
- Detailed implementation breakdown + second review round folded in
- Branch policy — reset stages land on reset/main; master never regresses
- Status header — Stages 0-1 done, Stage 2 in progress, map-value seam direction decided
- **reset**: Resequence Stage 3 (unions) to execute last, after Stage 6
- **reset**: Keystone-first plan — execution order 0→1→2→6a→3→4→5→6b
- **reset**: FromJson parses to a normal record, not a dictionary (Linus)
- **reset**: As-built status + honest call-axis verdict; architecture-complete at parity
- **reset**: ADR-069 — representation reset as-built; supersede ADR-062
- **reset**: SPECIFICATION.md as-built — AnyVal (née Json) + type-determined record repr
- Update stale ADR-062 references to ADR-069 post-reset
- **check**: Update intrinsics.rs comments Json→AnyVal post-reset
- **check**: Update remaining Json→AnyVal in checker helper comments
- FINDINGS.md for Explore E cleanup sweep
- **proposal**: Numeric-key (sparse Int) hashmaps + structural-key follow-on
- **spec**: SPECIFICATION.md §5.1.1 — numeric {Int:T} map keys
- Consolidate the representation-reset + numeric-key docs
- Rename Json type to AnyVal across all docs
- **site**: Rename Json -> AnyVal throughout docs-site source
- Post-reset quality/perf/cleanup plan + Wave A/J landing status
- Fix stale ownership_verify comment, ADR-069 as-built addendum, CLAUDE.md monomorphizer note
- **todo**: Add Wave M — investigate 25GB RSS vs Node 2-4GB
- **todo**: Wave M root cause — 265M live allocs (~100x amplification), allocator ruled out
- **todo**: Wave R measured findings (interning ruled out, contiguous=Node-matched, cache vs SMI) + new areas
- **todo**: Mark Waves A/J/A4/B done up top — only Wave R + #8 + B2 open
- **todo**: Check off all completed items [x] (Wave A/J/B/M); open = Wave R + #8 + B2
- **todo**: Sequence remaining work (Phase R0 parallel / R1 serial) + 9 additional areas to explore
- Arena-allocator feasibility investigation (~17%/4GB, ~0% speed; representation is the headline)
- **todo**: Restore 0xFE inline as priority; LinMap=15GB headline + INITIAL_CAP -1.4GB; note J4 incomplete (Json in test crates) + workspace-test lesson
- Peak-memory finding (maps=76% are materialized records), Wave-R lane status, arena + interp call-axis design docs
- **todo**: 0xFE (lane F) merged to master c2f77121
- **todo**: Value-unbox (#16) merged a63e9603 with churn-fix
- **todo**: Interp leak investigation (34MB/1.49M allocs/run, pre-existing) + agent
- **todo**: SMI is inert (box never emits immediates); enabling for real on feat/enable-smi
- **todo**: SMI enabled+working behind flag a1ba97cb; toggle-removal in progress
- **todo**: Interp leak fix merged 05140712; SMI toggle-removal blocked on guard whack-a-mole (leave flagged)
- **todo**: Clean up — drop merged Wave A/J/B detail + stale Wave R notes; concise current-state
- **todo**: SMI dropped (51febe63, on reference/smi); explore lane results (arena/interpd/columnar/sso)
- PERFORMANCE.md §5.7 (2026-06 memory+interp deep-dive: what worked, sound-but-0%, SMI round-trip, freeze direction) + correct §4 map attribution; TODO cleanup (drop header-compaction/B2/#8/Option-D, freeze is the primary lever)
- **todo**: Freeze-repack (46cc61f7) + columnar (20876032) merged + verified
- Fold design-* + investigation-arena + perf TODO into PERFORMANCE.md §5.7; delete them
- **async**: Clarify why parallel's tasks stays AnyVal (heterogeneous thunk-or-promise array, exempt from Function rejection)
- ADR-086/087, stdlib keys any-map test, integration tests for both bugs
- Document while(() => Boolean) in tutorial + lin-engineer agent
- **site**: Restructure into Learn/Guides/Reference split + fill recent-feature gaps
- **site**: Lead homepage with dot application; rework Why-Lin bullets
- Add utility-types reference page to docs-site
- Compiler coherence consolidation proposal + to-do list
- **coherence**: Mark Phase-0 landed (RC verifier, RAPTOR corpus+CI, capture dedup)
- **coherence**: Phase 1 (Cluster 3) DONE + RC verifier promoted to strict CI gate

### Features

- Change if syntax — then at end of condition line, else at if level
- Change if syntax — then at end of condition line, else at if level
- Error on old-style indented then in if expressions
- Add lin fmt source formatter with LSP integration
- **stdlib**: Redesign std/fs API with Unix-style naming and new operations
- **lang**: Bitwise operators & | ^ << >> and unary ~
- **lang**: Unboxed flat arrays for small int types + byte literals
- **coverage**: Multi-region line/branch coverage on the LinIR path
- **stdlib**: Std/net — UDP and TCP sockets (Milestone 21 Layer 2)
- **stdlib**: Std/proc + std/tty (Milestone 21 Layer 3)
- **stdlib**: Std/time.sleepMicros + std/signal.waitSignal (Milestone 21 Layer 4)
- **parse**: Line-leading [ / ( starts a new statement in inline bodies
- **async**: Phase 0-1 — fault isolation foundation + ADR-042
- **async**: Phase 2 — real async/await on OS threads (spawn-per-call)
- **async**: Phase 3 — parallel + real race/timeout/retry combinators
- **async**: Phase 4 — real bounded ThreadPool + poolAsync
- **async**: Phase 5 — real Worker (long-lived thread + mailbox)
- **async**: Phase 6 — Shared<T> opt-in shared mutable state
- **async**: Phase 7 — Frozen<T> opt-in shared read-only state
- **async**: Phase 8 — hardening, TSan leg, docs
- **async**: Close §32.2.3 nested-promise flatten + §32.2.2 Error type / is Error
- **lang**: Add prefix logical-not operator !
- **check**: Shared<T> accessor-only enforcement (Type::Shared variant)
- **json**: FromJson type-directed decode + close Json->concrete cast hole (ADR-046/047)
- **http**: Real serve intrinsic + fix imported-fn-as-value lowering
- **check**: Singleton string-literal types (ADR-051)
- **check**: Imported types usable in type position
- **json**: FromJson validates string-literal field values (ADR-052)
- **vscode**: Expose bundled lin on PATH
- **is**: Deep type validation for is <ObjectType> (ADR-053)
- **generics**: Monomorphized generic function values (Phase 0)
- **stdlib**: Consolidate std/proc into std/process (batch + streaming)
- **generics**: Harden single-module monomorphization (Phase 3.5)
- **stdlib**: Implement std/time format, fromIso, parse
- **generics**: Element-type-aware flat array write path (Phase 4.5)
- **generics**: Phase 4.5b — flow-type intermediate alloc element type for flat arrays
- **stdlib**: Genericize array at/set/indexOf to <T>(T[], …) (Phase 6, ADR-067)
- **async**: Enforce Error handling at await via T | Error (§32.2.2, ADR-065)
- Implement fixed-length array types ([T1, T2, ...], spec §8.3)
- **test**: Replace-based import mocking (ADR-071)
- **test**: WithFixture + report lifecycle helpers, testing docs (ADR-071)
- **stdlib**: Add std/yaml and std/jq modules
- Safe array.at + null-spread no-op; fix union-if type collapse
- **template**: Replace ${} substituter with minijinja + add layout system
- **streams**: Stage 1 — Type::Stream plumbing
- **streams**: Stage 2 — TAG_STREAM, LinStream, RC finalizer, lin_stream_read/close
- **streams**: Stage 3 — fs.openRead + file read backend + codegen dispatch
- **streams**: Stage 4 — std/stream adapters, sink, .drain()
- **streams**: Stage 5 — unify sources, .for(fn), must-use warning (sync-core checkpoint)
- **streams**: Stage 6 — affine use-after-move check + placement restriction
- **streams**: Stage 7 — CAP_MOVE resource handoff across the closure-env transfer ABI
- **streams**: Stage 8 — .promise() true-threaded driver + transform fault isolation
- **streams**: Cap lines() partial-line buffer (bounded-buffer backpressure guard)
- **streams**: Configurable lines() cap via linesMax(s, n)
- **iter**: Receiver-dependent combinator return typing (stream-aware)
- **iter**: Lazy stream backends + terminals; consolidate std/stream exports
- **docs-site**: GFM table support in the markdown builder
- **stdlib**: Std/compress streaming codecs + raw writeStream/writeLines split
- **stdlib**: Std/archive tar splitting (untar/manifest/files) + tar.gz example
- **fmt**: Opt-in column alignment for match arms and trailing comments
- **test**: JSON reporter mode for lin test
- **vscode**: Test Explorer integration + docs for --reporter json
- **stdlib**: ToJson recursive value serializer
- **test**: Versioned NDJSON schema + contract test
- **test**: Per-test timing in json reporter
- **test**: --filter-test to run individual tests by name
- **vscode**: Coverage run profile via lcov
- **imports**: Support cyclic imports via SCC type-checking
- **string**: Substring negative indices + optional end (default length)
- **check**: Number as a numerically-bounded monomorphized generic (ADR-018 reversed)
- **check**: Nested Number support — Number[] and combinator callbacks (bug #4)
- **stdlib**: Negative indices + default args across string/array
- Optional index parameter for iterator combinators
- **ffi**: Richer foreign imports — Ptr handles, $ORIGIN rpath, std/ffi raw-memory + real SDL3 examples
- **ffi**: MacOS @loader_path rpath + install_name fixup for vendored dylibs
- **check**: Stage 0.5 sealed-records — carry inert sealed marker through resolution
- **sealed**: Stage 2 — heap-field sealed records (String/Array/nested)
- **stdlib**: Make std/array.sort stable (merge sort), replacing quicksort
- **sealed**: Stage 3 — arrays of sealed scalar records (contiguous, unboxed)
- **stdlib**: Category 1 — element generics for std/array + std/iter collection ops
- **types**: Typed index-signature object type `{ String: T }` (ADR-082)
- **map**: Unbox flat-scalar values for { String: T } (ADR-082 follow-up)
- **sealed**: Stage 4 stack-alloc + RC-emission suppression (the records win)
- **stdlib**: Category 2 — type std/object map producers + groupBy/countBy over { String: T }
- **check**: Flow-narrow T | Null complement on null-test guards
- **stdlib**: Defaulted accessors object.get + array.atOr; use in RAPTOR
- **stdlib**: Make push/append/prepend generic <T>(arr: T[], item: T)
- **check**: Require a type annotation for an evidence-free empty collection literal
- **stdlib**: Generic push/append/prepend <T>(arr: T[], item: T) — close element-type hole
- **stdlib**: Generic <T> sort/sortBy/minBy/maxBy callbacks
- **stdlib**: Unify array.at/atOr and object.get into one T|D accessor
- **check**: Generalize flow-narrowing to any is-arm complement
- **check**: Enforce lin_* intrinsics are stdlib-only (ADR-086)
- **version**: Adopt single workspace version 1.0.0
- **types**: Record intersection & (ADR-061)
- **ir**: Representation-inference pass repr.rs as Stage-2 side-table observer
- **ir,codegen**: Stage 3 — codegen trusts func.repr at DECIDE/ASSUME sites
- **ir,codegen**: Stage 4 — BoxKeepPacked/UnboxKeepPacked + keep-packed Map round-trip
- **codegen**: Part C (partial) — IndexSet RHS + Release dispatched on func.repr
- **check**: Stage 0 unboxed-sumtype checker prereqs (gaps 2 & 3)
- **runtime,ir,codegen**: Unboxed-sumtype Stage 1 foundation (SumNode repr, gated INERT)
- **examples**: Fold streams/wordcount/indexed into report
- **ir,codegen**: Unboxed-sumtype Stage 1 LIVE — pack non-recursive scalar sum types end-to-end
- **stdlib**: Add std/event — typed event emitters (async worker + sync bus)
- **examples**: Fold codec into raspberry-controller (TLV telemetry + bit helpers)
- **lsp**: Add references, documentSymbol, documentHighlight, rename
- **lsp,check**: Record def_span for val/var/destructuring bindings
- **lsp**: Inlay type hints for inferred bindings
- **lsp**: Type-aware semantic tokens (full document)
- **lsp**: Signature help inside call argument lists
- **lsp**: Quick-fix code actions for unused imports and did-you-mean
- **lsp**: Cross-file references, rename, and workspace symbols (Tier 3)
- **vscode**: Enable inlay hints and semantic highlighting by default for .lin
- **examples**: Fold matrix+ffi into sdl
- **lsp**: Codelens run-test on test declarations
- **lsp**: Folding ranges and selection ranges
- **lsp**: Document links on import paths
- **lsp**: Auto-import quick-fix and import-path completion
- **lsp**: Signature help shows parameter names
- **vscode**: Add snippets, file icons, and getting-started walkthrough media
- **vscode**: Wire menus, tasks, problem matcher, walkthrough, snippets, file icon, runTest CodeLens command
- **lex,parse**: Make `from` a contextual keyword
- **examples**: Fold config+dijkstra into web-server (maps/routing service)
- **ir,codegen,runtime**: Unboxed-sumtype Stage 2 — recursive sum types pack as unboxed SumNodes
- **lsp**: Re-check open dependents when an imported file changes
- **parse**: Add additive full_span to compound Expr nodes
- **lsp**: AST-precise folding and selection ranges via full_span
- **codegen,runtime**: Keep-packed sum value through record fields (TAG_SUMNODE)
- **runtime,codegen**: ADR-063 Stage 3b mechanism (i) — named-descriptor materialize-on-read for 0xFE sealed arrays
- **lsp**: Cross-file goto-definition for imported symbols
- **ir**: Thread source spans to instructions
- **codegen**: Emit DWARF line tables under --debug
- **compile**: Wire --debug through the pipeline
- **cli**: Add --debug/-g build flag
- **vscode**: Lin debug configuration via CodeLLDB
- **vscode**: Lldb pretty-printers for Lin runtime values
- **check**: Thread var binding name into TypedStmt::Var for DWARF locals
- **ir**: Add DebugDeclare instruction carrying binding names
- **codegen**: Emit DILocalVariable for Lin locals under --debug
- **repr**: Widen sealed-array packability gate to String (ADR-063 Stage 3b)
- **repr**: Widen sealed-array packability gate to Array fields (ADR-063 Stage 3b)
- **repr**: Widen sealed-array packability gate to nested sealed-record fields (ADR-063 Stage 3b)
- **lsp**: Extract and render JSDoc-like doc comments
- **lsp**: Show doc comments in hover, completion, signature help
- **sealed**: Add KIND_MAP/NKIND_MAP heap-field kind for { String: T } map fields
- **std/string**: Add O(1) lengthBytes; use it to drop csv's re-declared string intrinsics
- **check**: Actionable errors for bad imports and missing modules
- **lsp**: Offer unimported stdlib combinators in dot-completion with auto-import
- **generics**: Generic reduce over heap elements + tuple-literal inference; type min/max/fromEntries
- **lsp**: Classify completion cursor context + collect in-scope binders
- **lsp**: Gate completion sources by cursor context + offer unreferenced binders
- **stdlib/array**: Make sum/product generic (<T>(arr: T[]): T)
- **lsp**: Insert parens and place cursor when completing a function
- **types**: Allow type aliases that resolve to String as map keys
- **lsp**: Type-match receiver against first param in dot-completion
- **lsp**: Offer unimported userland exports with auto-import
- **lsp**: Match tuple-args receiver against leading params in dot-completion
- **check**: Record lambda parameter types in span_type_map for inlay hints
- **lsp**: Inlay type hints on unannotated lambda parameters
- **vscode**: Granular lin.inlayHints settings gating variable/parameter hints
- **stdlib**: Tighten 5 mis-typed Json returns to their real shapes
- **stdlib**: Type http fetch + fs stat returns to their real shapes
- **stdlib**: Type net + fs content-function returns to their real X|Error shapes
- **stdlib**: Type fs + csv options bags as records with | Null optional fields
- **stdlib**: Type process + stream X|Error returns to their real shapes
- **stdlib**: Type crypto.newHasher, template render, fs.readChunk to real shapes
- **stdlib**: Type io.readLine/prompt as String | Null (EOF)
- **stdlib**: Type parse-result returns as Json | Error; fix docs-builder render narrowing
- **stdlib**: Make Shared<T> a properly-typed generic opaque handle
- **async**: First-class Promise<T> opaque handle type
- **stdlib**: Make sortBy/searchBy/dedupBy key-extractor callbacks generic over key type K
- **lang**: Add null-coalescing operator `??` (ADR-065)
- **check**: Symmetric seal propagation for nested sealed-record(-array) fields
- **check**: Lambda-set metadata on Type::Function + LIN_LAMBDA_STATS census
- **lin-ir**: Port leg1 ownership conventions + shadow-mode verifier onto master
- **archive**: Add composable TarEntry handles — entries/header/body API
- **bench**: Port RAPTOR GTFS loader to tar.gz nested streaming (gtfsLoaderTar)
- **bench**: Tar.gz streaming loader for the typed RAPTOR port
- **repr**: Stage-1 sealed-record arrays — pointer-backed share-on-push (0xFD)
- **ir**: D3a anon-structural param monomorphisation — share-on-pass for packed records
- **ir**: D3b anon-structural non-param slot project-copy
- **ir**: D3b return + closure-boundary + full slot test coverage
- **check**: Total literal-key index + literal-union refinement
- **repr**: Stage-2a — heap-field record arrays pointer-backed (0xFD) + map-value seam fix
- **ir**: D3 cross-module anon-structural-param monomorphisation
- **check**: { LiteralUnion: V } index-signature expands to a fixed record
- **check**: Narrow null-tested compound index places + reject non-String map keys on read
- **check**: Index/field assignment evaluates to the assigned value
- **runtime**: Introduce TAG_RECORD (Stage 6a leg 1) — sealed-struct-by-pointer in dynamic slots
- **runtime**: Stage 6a leg-3 — fromJson builds TAG_RECORD, not LinObject
- **ir,codegen**: Stage 7 NullableRecord — T|Null sealed records as nullable pointers
- **ir,runtime**: Stage 4 — RC traffic measurement + rc_elide coverage fix
- **stdlib**: Add integer-limit constants (MAX_UINT32, MIN_INT8, …) to std/number
- **reset**: Rename the dynamic top type Json -> AnyVal (Stage 6b, part 1)
- **reset/6b-linobj**: Convert process ExecResult producer to LinMap
- **reset/6b-linobj**: Convert url parse-result producer to LinMap
- **reset/6b-linobj**: Convert server route-params and request producers to LinMap
- **reset/6b-linobj**: Convert regex named-captures sub-map to LinMap
- **reset/6b-del**: Leg1 — TAG_MAP dual-dispatch in has_field/validate/union_get
- **reset/6b-del**: Leg2 fs — make_filestat/make_error → LinMap + stat/lstat reconstruct
- **reset/6b-del**: Leg2 os — lin_os_mem_info → LinMap + memInfo reconstruct
- **reset/6b-del**: Leg2 net — make_len_addr_port/make_fd_addr_port → LinMap + reconstruct
- **reset/6b-del**: Leg2 time — lin_time_components → LinMap + components reconstruct
- **reset/6b-del**: Leg2 stream — make_meta_object/FilesSource/record_to_object → LinMap + update unit tests
- **reset/6b-del**: Leg2 http — make_response_object/make_error_object → LinMap + fetch/fetchWith/postJson reconstruct
- **reset/6b-del**: Leg2 async_rt — make_error_tagged → LinMap + is_error_value handles TAG_MAP
- **reset/6b-del**: Leg3 env — lin_env_environ → LinMap (genuine dict, no reconstruction needed)
- **reset/6b-del**: Leg1 addendum — lin_union_force_to_object TAG_MAP arm (missed from Leg1 commit)
- **check**: Type::Map{key,value} + Int key resolution
- **runtime**: LinMap key_kind + int hash/eq + lin_map_get_int/set_int
- **test**: Add {Int32: T} numeric-key map integration tests + example
- **check**: Type::IntLit integer literal union type
- **examples**: Int-lit-types example + integration tests
- **check**: Infer int-map literal { 1: v, -1: w } as { Int32: T }
- **codegen**: Dispatch lin_map_set_int for integer-keyed map literals
- **runtime**: LinMap parity ops for LinObject deletion (Stage 6b phase 1)
- **codegen**: Track A concrete open-object flip LinObject* → LinMap* (Phase 2)
- **stdlib**: Std/datetime immutable calendar library + 3 compiler soundness fixes
- **stdlib**: Weekday literal-union + narrowTo* Int64 narrowing casts
- **checker**: Allow closed int-literal unions as index-signature keys
- **stdlib**: Add parseUInt8/16/32/64, parseInt64, parseFloat32 + tryParse* variants
- **check**: Hoist inner function literals within block scope
- **stdlib**: Std/datetime fixed-offset OffsetDateTime (Tier-1 timezones)
- **check**: Accept integer-literal-keyed object literals against int-literal-union records
- **reset**: Cluster A — migrate tagged_as_object consumers to read maps directly
- **reset**: Cluster B — lin_sumnode_materialize returns LinMap, fix callers
- **reset**: Cluster C — CAP_OBJECT uses LinMap (not LinObject) for release/clone
- **reset**: Cluster D part 1 — remove TAG_OBJECT producers from runtime + dead codegen fields
- **reset**: Cluster D — delete object.rs, remove all TAG_OBJECT infrastructure
- **lang**: Retire the Json type alias — AnyVal is the sole dynamic top type
- **runtime**: Add opt-in mimalloc global allocator behind cargo feature
- **lsp**: Colour function-typed val/var definition names as `function`
- **lsp**: Emit uniform function semantic tokens for dot-method call names
- **ir,codegen**: 0xFE inline sealed-record arrays (Phase 1, escape-gated)
- **ir,codegen**: 0xFE inline sealed-record arrays (Phase 2, container-escape-ok)
- **runtime**: Add LIN_SMI_STATS=1 instrumentation to alloc_tagged
- **runtime**: Add SMI infrastructure behind cargo feature `smi` (default OFF)
- **columnar**: Wire Phase-1 columnar (0xFC) record arrays into codegen
- **frozen**: Freeze repacks 0xFD pointer-backed record arrays to 0xFE inline
- **lsp**: Add Organise Imports source action (SOURCE_ORGANIZE_IMPORTS)
- **check**: Function overloading by parameter types (ADR-074)
- **check**: Cross-module function overloading (ADR-074)
- **check**: Numeric-conversion tie-break for overload resolution (ADR-075)
- **stdlib**: Fold number narrowTo* into to* overloads (ADR-075)
- **lsp,vscode**: Test UX improvements — suite CodeLens, grouped tree, runSuite cmd, result surfacing (A-E)
- **check**: Add TypeScript-style drill-down for type-mismatch diagnostics
- **check**: Name-preserving display for map-keyed type aliases
- **check**: Extend mismatch drill-down to assignment/annotation sites
- **stdlib,ir**: Condition-only while(() => Boolean) loop overload (ADR-081)
- **check**: Type BigInt/Decimal/Regex as opaque handles instead of AnyVal
- **check**: Drive generic inference from the call's expected result type
- **parse,check**: Destructuring lambda parameters (bare + parenthesized)
- **check**: Error on inner-scope variable shadowing
- **check**: Narrow index places after assignment; admit stable place-path keys
- **ir**: Auto-vivify intermediate maps on nested index-assignment
- **stdlib**: Keys() returns the map's native key type K[]
- **check**: Add TypeScript-style utility types (Partial/Pick/Omit/...) + keyof
- **check**: Narrow through compound && / || / ! conditions in if branches
- **parse/check/ir/codegen**: Add spread elements in array literals
- **stdlib**: Add callback-taking std/object.entries overload
- **ir**: Static RC-balance verifier over LinIR (LIN_VERIFY_RC, off by default)
- **ci**: Promote the RC-balance verifier to a strict CI gate

### Other

- Initial spec
- Implement lin-lang interpreter with stdlib and test suite

Complete tree-walking interpreter for the lin language specification:
- Hand-written lexer with indentation tracking (INDENT/DEDENT)
- Pratt parser for expressions, recursive descent for declarations
- Dynamic typing with runtime type tags for is/has checks
- Partial application, dot-chaining, closures with mutable capture
- Pattern matching (is type/literal, has shape, when guards)
- String interpolation, safe bracket access (null propagation)
- Module system with embedded stdlib (io, string, number, array, iter)
- 22 integration tests, 9 example programs all passing
- Fix postfix parsing after DEDENT, expand test suite to 60 tests

Fix a parser bug where `[x]` at block level after an indented function
body was incorrectly parsed as index access. The postfix loop now
suppresses `[` and `(` when the previous token was a DEDENT.

Add 38 new integration tests covering: negative literals, assignment
as expression, non-exhaustive match errors, is/has as boolean exprs,
string escapes, block expressions, dot partial application, boolean
negation, string/numeric comparison, logical operators, if block
branches, IEEE 754 floats, null propagation, comments, mixed numerics,
array pattern matching, object rest destructuring, over-application
errors, modulo, shared mutable closures, recursion, iter builtin, and
nested data access.
- Support continuation lines (&&/||) across newlines in parser

The parser now looks past Newline tokens for && and || operators,
matching the lexer's continuation-line behavior (suppressed INDENT on
lines starting with && or ||). This enables multi-line conditions:

  val x = a >= 18
    && active

Also adds 6 new tests for continuation lines, import aliasing, tuple
dot application, and array rest destructuring (66 total).
- Add data pipeline and custom iterator examples, improve CLI

New examples demonstrate:
- data_pipeline.lin: tagged unions, pattern matching, chaining, safe
  bracket access, closures in a realistic data processing scenario
- custom_iterator.lin: iter() built-in for Fibonacci sequences and
  countdown iterators with closure capture

CLI now supports reading from stdin with `lin -` and shows the
filename in error messages.
- Implement tail call optimization for direct self-recursion

TCO uses a TailResult enum to detect self-recursive calls in tail
position (if/else branches, block final expressions, match arms) and
loops instead of creating new stack frames. sum(100000, 0) now runs
in constant stack space.

Adds 2 TCO tests (68 total).
- Expand string stdlib with contains, startsWith, endsWith, split, join, replace

Add 6 new string intrinsics and their .lin wrappers. Also add Span::line_col
helper for future error message improvements. Test suite now at 72 tests.
- Add push, concat, keys, values, entries intrinsics

Array operations (push mutates, concat creates new) and object
introspection (keys, values, entries) for working with JSON data.
74 tests passing.
- Implement map/filter/reduce in .lin stdlib using for+push

The array stdlib now uses the language's own for loop and push to
implement map, filter, and reduce — demonstrating the "thin runtime,
fat stdlib" principle. The global built-in versions remain as fallbacks
for direct use without imports.
- Implement range and iterOf in .lin using iter built-in

Both now produce proper opaque Iterator values instead of materialized
arrays, matching the spec's design for lazy iteration. The global
built-in range (which materializes) remains as fallback for direct use.
- Fix multi-statement lambda bodies inside parentheses

Lambda bodies like `x => val y = x * 2; y` inside .for() or .map()
now parse correctly. The parser detects val/var tokens as the signal
for a multi-statement body when INDENT/DEDENT are suppressed by
balanced delimiters.

76 tests passing.
- Add forward references, user module imports, and stdlib array functions

- Forward references between top-level functions via mutable cell pre-scan
  (enables mutual recursion without ordering constraints)
- User module loading from filesystem (relative path resolution)
- New stdlib/array functions: find, some, every, flatMap, indexOf, reverse
- Showcase example demonstrating full language capabilities
- Multi-file example demonstrating user module imports
- 82 integration tests (up from 76)
- Fix multi-line import statements

The lexer left at_line_start=true when inside balanced delimiters,
causing spurious INDENT tokens after closing braces. Now at_line_start
is always reset, preventing stale state from triggering indentation
handling after returning to unbalanced context.
- Add line numbers to runtime error messages

Key errors (undefined variable, undefined function, array OOB, cannot
call) now include [line:col] position from the source span, making it
much easier to locate problems in larger programs.
- Add charAt, repeat string functions and __stringRepeat intrinsic
- Document ADR-017: lexer at_line_start reset fix
- Add compiler, vscode extension
- Compiler
- Compiler improvements
- Remove interpreter, implement missing compiler functionality
- Use stdlib in stdlib where possible, remove globals, fix numerous issues, add test functions, add templating
- Add to stdlib
- Build process, compiler options, memory management
- Fix optional chaining
- Add object assignments
- Self-contained VSIX with dot-completion, auto-import, and bundled binaries
- Simplify files glob to dist/* to fix softprops/action-gh-release
- Platform-specific VSIXs with marketplace publishing
- Fix VSIX version stamp (export PATCH env var for node)
- Add hash stdlib
- Initial logo
- More memory management
- Make marketplace publish non-blocking so artifacts always upload
- Memory management
- Add sleep after tag delete to avoid race on re-create
- Add dist listing step to debug release upload
- Replace softprops action with gh CLI for rolling release
- Write release notes to file to avoid YAML quoting issues
- Use --cleanup-tag on gh release delete so tag is also removed
- Remove continue-on-error from marketplace publish to surface real errors
- Fix command titles and add onCommand activation events
- Fix dot-completion returning empty list when receiver type unresolved
- Fix vsce publish -- drop --target when using --packagePath
- Update gitignore and ext lock
- Merge branch 'worktree-agent-ac4f4f64170108994'
- Include node_modules in VSIX -- vscode-languageclient was being excluded
- Update devcontainer
- Bundle node_modules, add icon, bump to 0.3.1, fix --no-dependencies
- Icon for vscode extension
- Update workflow
- Add matrix, dijkstra, and web-server multi-file projects
- Phase 4: cross-block RC elision in LinIR pipeline

Extend the Perceus-style RC elision pass to find Retain/Release pairs
that span multiple basic blocks, using BFS over CFG successors (capped
at BFS_BLOCK_LIMIT=8 blocks to bound compile-time cost).

- Activated the previously-unused liveness analysis (renamed _liveness → liveness)
- Added block_index HashMap for O(1) BlockId → index lookup
- Renamed find_paired_release to find_paired_release_in_block (same-block only)
- Added find_paired_release_cross_block: BFS over successors to find the
  Release when the Retain is at the end of a block
- Added helpers: temp_survives_to_block_end, find_release_at_block_start,
  block_is_clean_for, block_temp_survives, terminator_successors
- 4 new unit tests covering cross-block elision scenarios
- Updated docs/MEMORY_MANAGEMENT.md Phase 4 status to complete
- Merge phase4: cross-block RC elision in LinIR pipeline
- Update TODO
- Phase 2b: fix 5 RC leak gaps in codegen

Gap 1 (var reassignment): release old heap value from alloca before
overwriting in compile_local_set, guarded by a non-null check.

Gap 2 (var scope-exit): track heap-typed Var slots in Block compilation
and release the current alloca value at scope exit, skipping heap_var_slots
(closure-shared cells) and the result slot.

Gap 3 (mixed if/match ownership): when one branch of an if or match
produces an owned alloc and another produces a borrowed reference,
retain the non-owned branch before its unconditional branch to the merge
block so both branches arrive at the PHI with consistent ownership.

Gap 4 (match scrutinee): after the merge phi in compile_match, release
a fresh scrutinee allocation when it is a concrete heap type (skips
union/TypeVar scrutinees handled by the switch path).

Gap 5 (string interp TypeVar temps): mark TypeVar/Union interpolation
parts as is_fresh=true in compile_string_part_owned because
lin_tagged_to_string always returns a freshly allocated LinString*.

All 128 integration tests pass.
- Merge phase2b: complete Phase 2 RC emission gaps in codegen
- Merge fs-api-improvements: redesign std/fs API with Unix-style naming
- Phase 0: capture IR-path parity baseline ledger (121/128 failing)
- Phase 1a: resolve imported-function/val calls in IR lowering

Import bindings were lowered to dead placeholder temps, so every call to an
imported symbol (including print) vanished. Resolve import/foreign-import slots
to CallTarget::Named(<mangled symbol>) and imported non-fn vals to their
zero-arg __val wrappers, boxing concrete args passed to Json/union params.

IR-leg integration: 7 -> 45 passing. AST leg unchanged (128/128).
- Phase 1b: fix top-level function FuncId assignment in IR lowering

Top-level function vals were lowered with a freshly-allocated FuncId instead of
the one pre-assigned in global_fn_slots, so CallTarget::Direct referenced a
non-existent function and panicked. Reuse the pre-assigned id, and allocate
main as FuncId(0) so codegen names it 'main'.

IR-leg integration: 45 -> 51 passing. AST leg unchanged (128/128).
- Phase 1c: ToString dispatch by input type, not LLVM value kind

compile_to_string_value dispatched on the LLVM value kind, so a Str (a pointer)
was treated as a tagged ptr and stringified as garbage (broke string interp).
Thread argument static types into compile_ir_intrinsic and delegate ToString to
the type-driven value_to_string_simple.

IR-leg integration: 51 -> 54 passing. AST leg unchanged (128/128).
- Phase 1d: carry object/key types on IR Index; unbox boxed containers

The Index instruction carried only the result type, so indexing a Json-boxed
object (e.g. stat(path)["size"]) called lin_object_get directly on the
TaggedVal* and returned null. Add obj_ty/key_ty to Index; in compile_ir_index
unbox union/TypeVar containers and boxed keys before the runtime accessors, and
dispatch array-vs-object by object type. Unblocks the fs/http/server cluster.

IR-leg integration: 54 -> 67 passing. AST leg unchanged (128/128).
- Phase 2: lower IndexSet + fix flat-array index reads in IR path

Add an IndexSet IR instruction and compile_ir_index_set (object/array/dynamic
dispatch, mirroring AST compile_index_set). Fix compile_ir_index to read flat
scalar arrays via flat_array_get and Json results via lin_array_get_tagged,
instead of always treating elements as TaggedVal* (which produced garbage for
flat Int32 arrays after assignment).

IR-leg integration: 67 -> 69 passing. AST leg unchanged (128/128).
- Phase 4 (pre): scope if-branch owned temps to their own branch

lower_if registered heap temps allocated inside then/else branches in the
enclosing scope, so the merge block released BOTH branches' temps — but only one
branch executed, freeing undefined values (SIGABRT / misaligned-pointer release).
Give each branch its own ownership scope, releasing branch-local temps before the
jump to merge and keeping the branch result. Surfaces the loop-RC risk class early.

AST leg 128/128; IR leg 69 (no regression; fixes latent heap corruption).
- Phase 2c: carry operand type on IR Binary for eq/cmp dispatch

The Binary instruction stored only the result type, so == and comparisons saw
lty=Bool and did scalar comparison instead of object/array deep equality
({..}=={..} returned false). Add operand_ty to Binary and pass it to
compile_binary_op_values as the operand type.

IR-leg integration: 69 -> 75 passing. AST leg unchanged (128/128).
- Phase 4a: add Phi instruction for SSA merges in IR path

The IR codegen used a flat temp_map with no SSA merge: lower_if copied both
branches into a shared result temp, and the single-pass codegen let the
last-compiled branch's value win for BOTH runtime paths (if-expressions returned
wrong values / crashed). Add a Phi IR instruction recording the actual
predecessor block per branch, lowered to an LLVM phi. This is the value-merge
infrastructure the loop lowering also needs.

IR-leg integration: 75 -> 83 passing. AST leg unchanged (128/128).
- Phase 4b: wrap capture-less functions in closure ABI on MakeClosure

A named/top-level function (no captures) is compiled without an env param, but
closure call sites invoke fn_ptr(env, args...). Storing the raw fn ptr shifted
all args by one (apply(add,7) returned 7 instead of 14). For capture-less
MakeClosure targets, emit an env-ignoring wrapper via wrap_named_fn_as_closure
and also defer phi backpatching so loop back-edges resolve.

IR-leg integration: 83 -> 87 passing. AST leg unchanged (128/128).
- Phase 4c: lower loops to explicit IR blocks (for/while/map/filter/reduce/range)

Option B core: control-flow intrinsics now lower to explicit LinIR basic blocks
(header phi + back-edge + body-closure call) instead of opaque runtime calls.
- Added ArrayAlloc / FlatArrayAlloc / FlatArrayPush intrinsic codegen.
- Callback ABI: box concrete args to Json params, coerce closure returns to the
  declared (boxed) return type, re-box union-operand arithmetic results.
- Fixed compile_ir_box/compile_ir_coerce to HEAP-box (lin_box_*) instead of a
  stack alloca, so boxed values that escape (returned / stored / captured) don't
  dangle — this was the [object] garbage bug.

IR-leg integration: 87 -> 92 passing. AST leg 128/128. Known remaining: a loop-RC
double-free segfault (values correct, exit cleanup corrupts the heap).
- Phase 4d: retain Function-typed args to balance callee consume

AST-compiled stdlib callees release their Function-typed parameters at return.
A closure passed from IR-compiled main was thus freed by the callee and then
released again at module-scope exit -> double-free segfault (surfaced with two
map/filter calls). Retain Function-typed args before Named/Direct calls so the
callee's release is balanced.

IR-leg integration: 92 -> 94 passing. AST leg 128/128.
- Phase 4e: match Phi merge, scrutinee boxing, Direct-call arg boxing, FieldGet obj_ty

- lower_match merged arms with a shared Copy (last arm won for all paths -> [object]);
  now emits a Phi and boxes the scrutinee so is/has tag tests work.
- Box concrete args to Json params for global (Direct) function calls, matching the
  imported-call path (fixes user fns taking Json, e.g. match-based describe).
- Add obj_ty to FieldGet; unbox boxed-object containers before lin_object_get (fixes
  destructuring a boxed Json param, which had regressed).

IR-leg integration: 94 -> 95 passing. AST leg 128/128.
- Phase 4f: literal patterns compare by value; fix Panic double-terminator

- is-literal patterns (is "Dave") were lowered as type-only checks, matching any
  same-typed value. Now emit a value equality: box the literal to Json and compare
  via lin_tagged_eq (declared with its real i8 return + truncate; an i1 decl read
  garbage bits and compared as always-true).
- Panic instruction codegen emitted build_unreachable AND the block's IR terminator
  also emitted one -> 'terminator in middle of block'. Drop it from Panic.

IR-leg integration: 95 -> 97 passing. AST leg 128/128.
- Phase 7: implement TCO TailCall terminator (structural loop transform)

The TailCall terminator was a build_unreachable stub, so deep self-recursion
stack-overflowed. Functions containing a TailCall now get a param-alloca prologue
(tco_entry) that stores incoming params and branches to the first IR block, which
becomes a loop header reloading params each iteration. TailCall stores the new arg
values into the param allocas and branches back. Post-tail-call continuation blocks
are marked 'diverged' so they are not treated as live phi predecessors.

count(1_000_000, 0) now returns 1000000 with no overflow.
IR-leg integration: 97 -> 98 passing. AST leg 128/128.
- Phase 6: partial application in IR Call handler

Under-applied Direct/Named calls (fewer args than arity, Function result type) now
build a partial-application closure via build_partial_application_values (a
value-input port of the AST build_partial_application). add(5)(10) -> 15.

IR-leg integration: 98 -> 100 passing. AST leg 128/128.
- Phase 6b: unbox narrowed Json slots on LocalGet

A Json parameter narrowed to a concrete type inside a match arm (is String => x)
read the boxed TaggedVal* without unboxing, yielding null/garbage. LocalGet now
emits a Coerce when the slot's stored type is union/Json but the use site wants a
concrete type.

IR-leg integration: 100 -> 101 passing. AST leg 128/128.
- Phase 6c: closure env captures (EnvCapture) + uniform boxed closure return ABI

Closures capturing values crashed two ways: (1) env reads were lowered as
lin_object_get on the raw env struct (wrong) -> added an EnvCapture IR instruction
that loads from byte offset 8+index*8, matching make_closure_struct; (2) a closure
returning a concrete scalar was invoked through the opaque-Function ABI which
expects a ptr return -> closures now use a uniform Json (boxed) return so any
Function value is callable without knowing its concrete return type.

Capturing-param closures (adder(10)(5)->15) work. Mutable var-capture-by-ref still
pending. IR-leg 101 passing. AST leg 128/128.
- Phase 5 (partial): IR codegen for async/await/exit intrinsics

Add Async/Await/Exit Intrinsic variants + lowering mappings + value-input codegen
(call_thunk_value runs a thunk closure via the uniform boxed ABI, wraps in a
promise; await unwraps). Note: stdlib async/await wrappers are AST-compiled today,
so these fire only once the IR path compiles imports too. No regression (101).
AST leg 128/128.
- Phase 4g: box concrete values in is/has and union-typed slots

- is/has expressions now box a concrete operand before the tag/shape check.
- Val/Var binding a concrete value to a Json/union slot coerces (boxes) it, so later
  reads see a TaggedVal* (e.g. val x: Json = 42; x is Int32).
- compile_ir_has_pattern unboxes the boxed object + guards the object tag.
IR-leg 102 passing. AST leg 128/128.
- Phase 4h: widen mixed int/float operands in IR binary ops

5 + 3.0 panicked (int_add on a float operand). compile_binary_op_values now
widens an int operand to f64 when the other side is float, for arithmetic and
comparison. IR-leg 102 -> 103 passing. AST leg 128/128.
- Phase 3/4: coerce array-literal elements to the element representation

MakeArray lowering now coerces each element to the array's elem_ty, so a
heterogeneous (Json) array boxes its concrete elements before pushing. Fixes
object toString and homogeneous arrays; a boxing edge for booleans inside
heterogeneous literal arrays remains. No regression (103 passing). AST leg 128/128.
- Phase 6d: complete uniform closure return ABI (indirect calls + boxed named-fn wrapper)

Closures return boxed Json, but indirect call sites built the call signature from the
expected concrete return type and named-fn wrappers forwarded a concrete return -> the
boxed ptr was read as a scalar (or vice versa), garbage/crash for curried and
higher-order calls. Now: indirect closure calls always use a ptr return and unbox to
the expected type; capture-less MakeClosure uses wrap_named_fn_as_closure_boxed which
boxes the wrapped fn's return. higher_order_functions, curried closures work.

IR-leg 103 -> 107 passing. AST leg 128/128.
- Phase 3b: array rest destructuring via lin_array_slice_tagged; auto-declare Named runtime calls

...rest now slices arr[elements.len()..length] via lin_array_slice_tagged instead of
copying the whole array. The IR Named-call path auto-declares unknown runtime symbols
from the arg/return types so such calls link.

IR-leg 107 -> 108 passing. AST leg 128/128.
- Phase 4i: array pattern matching (is [], is [a,b], is [x,...rest])

Array patterns were lowered as a type-only check against Type::Never (never matched).
Add an ArrayLenCheck IR instruction (value is-array AND length ==/>= n) and lower
Is(Array{..}) to it; element bindings reuse the Index path (which unboxes the boxed
scrutinee). IR-leg 108 -> 109 passing. AST leg 128/128.
- Phase 4j: has-array patterns + array-pattern rest binding

Has(Array) patterns use ArrayLenCheck like Is(Array); array-pattern rest binding
slices the remaining elements via lin_array_slice_tagged (unboxing the boxed
scrutinee first). has [first, ...rest] works. IR-leg 109 -> 110 passing.
- Phase 4k: object rest destructuring (ObjectRest instruction)

val { a, ...rest } = obj now binds rest to a new boxed object with all fields except
the destructured ones, via lin_object_copy_except. IR-leg 110 -> 111 passing.
- Phase 4l + 6e: lower iter() to IR blocks; partial-app wrapper returns boxed

- lower_iter: iter(init,cond,next,current) builds a Json array via an explicit loop
  (state phi carried as Json; the four callbacks invoked through the uniform closure ABI).
- build_partial_application_values now returns a boxed TaggedVal* and boxes the underlying
  call result, matching the uniform closure return ABI (a regression from making indirect
  calls expect ptr returns). Fixes iter_builtin, iterator_restart, and partial application.

IR-leg 111 -> 113 passing. AST leg 128/128.
- Phase 4m: coerce if-branch values to result type; has-pattern value constraints

- lower_if coerces each branch result to the if's result representation (e.g. an Object
  branch boxed to a Json if-result), so the phi inputs agree and a function returning
  Json from an if actually returns a boxed value.
- has { "k": lit, binding } patterns now check the field value equals the literal (AND-ed
  with field presence), not just presence.
Both regression-free. A separate RC-ordering bug (object string keys freed when a
returning function's scope exits) still affects match-on-returned-object. IR-leg 113. AST 128/128.
- Phase 4n: keep pre-coercion temp when boxing a function return

A function returning Json from a concrete body (e.g. an object) boxed the result but
the scope-release then freed the original object the box wraps -> dangling object on
return (crash matching a returned object). pop_scope_releasing_keep retains both the
boxed return and the raw pre-coercion temp.

IR-leg 113 -> 114 passing. AST leg 128/128.
- Phase 4o: mutable var captured by closure via heap cells (ADR-015)

A var mutably captured by an inner closure now lowers to a heap cell (MakeCell):
the slot holds the cell pointer, reads/writes go through CellGet/CellSet, and the
closure captures the cell pointer (EnvCapture loads it; the inner slot is marked a
cell). Pre-scan collects mutably-captured var slots. Counter closures (makeCounter)
now correctly share and mutate state. IR-leg 114 -> 115 passing. AST leg 128/128.
- Phase 4p: bind before guard; guard has-pattern on non-objects

- lower_match: emit pattern bindings BEFORE the guard, since guards reference them
  (has { name, age } when age > 30).
- compile_ir_has_pattern: branch on the object tag so lin_object_has is only called on
  actual objects (a string/other scrutinee no longer derefs a non-LinObject pointer).
pattern_matching_has works. IR-leg 115 -> 116 passing. AST leg 128/128.
- Phase 4q: coerce call args to param numeric width; equalize int widths in binops

- lower_coerce_arg: widen/narrow a concrete numeric arg to the callee's param type
  (e.g. Int32 literal 0 -> Int64 param), in addition to union boxing.
- compile_binary_op_values: sign-extend the narrower operand when integer widths differ
  (Int64 n vs Int32 literal 0 in n == 0), so ICmp/arith operands agree.
Unlocks Int64 TCO. IR-leg 116 -> 118 passing. AST leg 128/128.
- Phase 4r: fix bool/scalar + null boxing into tagged arrays

- tagged_array_push_value: zero-extend small scalars (bool/i8/i16/i32) to i64 before
  storing in the push cell, since lin_array_push copies a full 8 bytes ([true,false]
  no longer reads garbage -> [true,true]).
- lin_array_push_tagged: a null TaggedVal* is the Json null value -> store a TAG_NULL
  entry instead of dereferencing null (heterogeneous arrays with null no longer crash).
IR-leg 118 -> 119 passing. AST leg 128/128.
- Phase 4s: null-safe lin_unbox_ptr for chained index null propagation

obj[missing][key] crashed: indexing the resulting null container unboxed a null
TaggedVal*. lin_unbox_ptr now returns null for a null input, so safe bracket access
propagates null (spec 6.1) instead of dereferencing. speculative_reads works.
IR-leg 119 -> 120 passing. AST leg 128/128.
- Phase 4t: keep raw pre-coercion temp in if-branches (boxed-object RC)

lower_if's per-branch scope released the raw object that the kept boxed result wraps
(same class as the function-return fix). Use pop_scope_releasing_keep([val, raw]).
Single-block match-on-returned-object now correct; a multi-match compiler-side crash
remains. AST leg 128/128.
- Phase 4u: branchless has/arraylen-check; per-arm ownership scopes

- compile_ir_has_pattern / ArrayLenCheck now use branchless runtime helpers
  (lin_value_has_field, lin_value_array_len_check) instead of emitting internal LLVM
  branches — keeps each as a single basic block, avoiding out-of-order block creation
  that broke SSA dominance when used in match arms.
- lower_match: each arm body runs in its own ownership scope (keeping the result),
  so an arm's heap temps are released within the arm, not at enclosing-scope exit where
  only one arm ran. Also track per-IR-block exit LLVM block for phi predecessors.
- Multi-match programs (two match blocks) now compile/run; a remaining dominance issue
  affects match-on-if-returning-objects (tagged_unions). IR-leg 120 (stable). AST 128/128.
- Phase 4v: scope has-pattern value-constraint temps to the test block

The transient comparison temps (boxed literal, fetched field) in a has value-constraint
were registered owned in the enclosing scope and released at module exit — but they are
defined in a per-arm test block that doesn't dominate the exit (SSA violation). Scope
them so they release in the test block. tagged_unions works. IR-leg 120 -> 121. AST 128/128.
- Phase 4w: box == rhs by its value kind, not the (Bool) result type

x == 3 with x:Json boxed the literal 3 as a Bool (using result_ty), so lin_tagged_eq
saw mismatched tags and returned false. Box the rhs by its actual LLVM value kind via
llvm_value_concrete_type. Fixes stdlib some/every. IR-leg 121 -> 122. AST 128/128.
- Phase 4x: recognize any-width int key as array index

lines[0] with an Int32 literal index was treated as object access (key check only
accepted i64), passing an i32 to lin_object_get -> signature mismatch. Use key_ty
numeric / non-bool-int to detect array indexing. Fixes fs_read_lines.
IR-leg 122 -> 123. AST 128/128.
- Phase 4y: closures stored in arrays (unbox boxed callee + retain element)

- Indirect call: unbox a Json-typed callee (e.g. retrieved from arr[0]) to the closure
  struct before dispatch.
- MakeArray: retain Function-typed elements so the array's ownership is balanced against
  the element's scope-exit release (a closure stored in an array that outlives the
  constructing function no longer dangles).
Fixes closure-in-array and multiple_closures_share_var. IR-leg 123 -> 124. AST 128/128.
- Phase 5b: module-level vals as globals for closure access

A closure referencing a top-level val read a placeholder (the val lives in main's SSA
temps, invisible to closures). Top-level non-function vals are now published to per-slot
LLVM globals (GlobalValSet in main); closures load them via GlobalValGet. Fixes
async_val_capture and any closure over a module val. IR-leg 124 -> 125. AST 128/128.
- Phase 5c: register foreign-library link paths on the IR path

FFI calls failed at link time (undefined reference) because foreign_lib_paths is
populated by the AST compile_stmt, which the IR path skips. Collect ForeignImport
lib paths from the typed module in the IR branch of compile(). FFI works.
IR-leg 125 -> 126. AST 128/128.
- Phase 5d: void closures return void (not boxed Json)

A closure/function with a Null/Never return was forced to the boxed-Json ABI, emitting
Return(Some(value)) into a void LLVM signature (verifier error). Keep void returns void:
effective_ret stays Null/Never and the terminator is Return(None). Fixes async.test and
worker_request_reply. IR-leg 126 -> 127. AST 128/128.
- Phase 5e: tagged comparison ops + forced concrete return for callback args

- compile_binary_op_values: boxed (union) <,<=,>,>= operands use lin_tagged_cmp (handles
  strings and numbers), fixing string sort/comparison through Json.
- A closure passed as a callback argument whose parameter declares a concrete (non-union,
  non-void) return type is now compiled to return that concrete type (lower_call_arg ->
  lower_callback_arg), so AST-compiled higher-order callees receive a raw value.
- Also: box == rhs by its value kind (earlier), unbox boxed indirect callee.
No regression (127/128 integration). AST 128/128. groupBy/countBy with computed keys
still hit an AST-stdlib closure-ABI corner (stdlib-suite only).
- Phase 5f: only force concrete callback return when callback params are concrete

A callback with union/Json (TypeVar) params is called by AST stdlib via the
boxed-return convention (build_closure_call_typed boxes args + unboxes the ptr result),
so such closures must keep the uniform boxed ABI. Restrict the forced concrete return to
callbacks whose params are all concrete. Keeps string sort working; safe ABI match.
127/128 integration, 10/14 stdlib. AST 128/128.
- Phase 5g: capture-less anonymous closures use the uniform boxed return ABI

The uniform-boxed return was keyed on is_closure (has captures), so a capture-less
anonymous closure (e.g. groupBy's x => "all") returned its raw declared type. But it is
still invoked via the closure calling convention — including AST build_closure_call_typed,
which reads the result's payload at offset 8 — so a raw String made AST read the string's
data bytes as a pointer (crash 0x6c6c61 = "all"). Key the boxed ABI on 'anonymous'
(no pre-assigned FuncId) instead, covering capture-less closures. groupBy/countBy with
computed keys now work. No integration regression (127/128).
- **lin-ir**: IR-compile imported modules — partial (integration 128/128, stdlib 5/14)
- **lin-ir**: IR-compile imports — 128/128 integration, 11/14 stdlib
- Merge ir-imports: compile imported modules through the LinIR pipeline

Imports (the entire stdlib) now compile via lower_import_module + compile_import_from_ir
instead of the AST register_import path, removing the IR path's dependency on the legacy
AST compiler for imports. This unblocks Phase 10 (legacy deletion).

Parity: 128/128 integration and 14/14 stdlib on both legs; all stdlib ASan-clean on the
IR leg. ~15 RC/ABI/codegen fixes (cross-module FuncId prefixing, Is(Object) pattern field
checks, resolve_lin_str u64 discriminator, runtime key-tag Index dispatch, container
element ownership transfer, match-scrutinee transfer, full async family, etc.).
- Phase 9: flip default compilation path to LinIR

The LinIR pipeline is now the default for both the main module and imports. The legacy
TypedAST path remains behind a LIN_USE_AST=1 escape hatch (one-release deprecation window)
and is still used for coverage instrumentation (AST-only). CI matrix axis renamed to
backend: [ir, ast]; both legs green (128/128 integration, 14/14 stdlib each).
- Phase 10: delete the legacy TypedAST compilation path (Milestone 2)

LinIR is now the sole compilation backend. Removed the AST-direct codegen entirely:
compile_module/register_import/register_import_with_source/set_source and the ~60-function
private cascade (compile_expr, compile_call family, compile_intrinsic_call,
compile_async_intrinsic, compile_match family, try_tco_tail_call, the TypedExpr loop forms,
build_partial_application, compile_make_array/object, compile_if/index/index_set, value_to_string,
compile_function_body, ...), plus AST-only state (FnCtx, SlotStorage, TcoState, and the dead
Codegen runtime-fn/slot fields).

codegen.rs: 7,685 -> 3,322 lines (~5,900 removed). Removed the LIN_USE_AST escape hatch and
the CI backend matrix axis.

lin test --coverage is stubbed (errors 'temporarily unavailable') since its instrumentation
lived only in the AST path; CoverageEmitter scaffolding retained for a future IR port.

Gate: cargo test --workspace green (128/128 integration + all unit), stdlib 14/14, examples
run, stdlib ASan-clean. Docs (CLAUDE.md, ledger) updated to describe the single IR pipeline.
- Merge branch 'master' into lowlevel-bitwise
- **lin-ir**: IR-compile imported modules — partial (integration 128/128, stdlib 5/14)
- **lin-ir**: IR-compile imports — 128/128 integration, 11/14 stdlib
- Phase 9: flip default compilation path to LinIR

The LinIR pipeline is now the default for both the main module and imports. The legacy
TypedAST path remains behind a LIN_USE_AST=1 escape hatch (one-release deprecation window)
and is still used for coverage instrumentation (AST-only). CI matrix axis renamed to
backend: [ir, ast]; both legs green (128/128 integration, 14/14 stdlib each).
- Phase 10: delete the legacy TypedAST compilation path (Milestone 2)

LinIR is now the sole compilation backend. Removed the AST-direct codegen entirely:
compile_module/register_import/register_import_with_source/set_source and the ~60-function
private cascade (compile_expr, compile_call family, compile_intrinsic_call,
compile_async_intrinsic, compile_match family, try_tco_tail_call, the TypedExpr loop forms,
build_partial_application, compile_make_array/object, compile_if/index/index_set, value_to_string,
compile_function_body, ...), plus AST-only state (FnCtx, SlotStorage, TcoState, and the dead
Codegen runtime-fn/slot fields).

codegen.rs: 7,685 -> 3,322 lines (~5,900 removed). Removed the LIN_USE_AST escape hatch and
the CI backend matrix axis.

lin test --coverage is stubbed (errors 'temporarily unavailable') since its instrumentation
lived only in the AST path; CoverageEmitter scaffolding retained for a future IR port.

Gate: cargo test --workspace green (128/128 integration + all unit), stdlib 14/14, examples
run, stdlib ASan-clean. Docs (CLAUDE.md, ledger) updated to describe the single IR pipeline.
- Fix RC leak: release Function-typed params at callee exit

Every caller retains Function-typed arguments via retain_call_arg before passing
them (mirroring the callee's responsibility to release). When a Function param
was never read inside the body (no LocalGet, so never registered in the body
scope), the callee exited without releasing it, permanently leaking the rc
contributed by the caller.

Fix: push a dedicated param scope frame before the body scope in
lower_function_expr_with_id. Register each Function-typed explicit param in the
param scope. After the body scope is popped, pop the param scope — releasing any
Function param not being returned to the caller. The keep list is the same as the
body scope pop, so a Function param that IS the return value transfers ownership
to the caller unchanged.

Str/Array/Object params are not registered in the param scope because call sites
do not add an extra unregistered retain for those types; their RC lifecycle is
fully managed by the caller's scope.

All 128 tests pass.
- Merge master into param-release, resolve lower.rs conflict
- Fix RC leak: release Function-typed params at callee exit
- Merge branch 'master' into lowlevel-bitwise
- Merge master (bitwise ops, RC-leak fixes) into closure-abi-unbox
- Merge closure-abi-unbox: cache small-int/bool boxes — LinIR now faster than legacy AST

Eliminates the per-element TaggedVal heap alloc in map/filter/reduce callbacks by interning
small ints [-16,256) and bools as immutable static boxes. The LinIR path, which was ~10%
slower than the deleted TypedAST path, is now ~21% faster (release -O2; mallocs 11.5M->7.1M).
Memory-safe (lin_tagged_release skips cached boxes; ASan-clean) with unit tests for the
cache contract.
- Merge coverage-linir: multi-region line/branch coverage for lin test --coverage

Re-adds lin test --coverage on the sole LinIR backend (the legacy TypedAST
coverage path was deleted in Phase 10), now with line/region granularity
instead of the old function-level counters.

- ir: BasicBlock.span, populated for function-body/if/match/guard blocks
- coverage: CoverageEmitter generalized to N counters + multi-region covmap
- codegen: per-block counter increments, current_source threading, finalize once
- compile: main + non-stdlib import sources threaded; stdlib excluded
- cli test: --coverage rejection removed; instrumented binary kept for report
- Merge master into perf-elem-alloc
- Merge branch 'master' into lowlevel-bytes
- Fix RC leak: release old value on concrete-rc var/global reassignment
- Merge branch 'master' into lowlevel-bytes
- **lin-runtime**: Pin TaggedVal/LinArrayElem layout + guard refcount underflow
- Update todos
- Merge branch 'master' into review-hardening
- Merge branch 'review-hardening'
- Merge branch 'warning-cleanup'
- Add end-to-end benchmark harness for compiled Lin
- Force fresh lin-runtime rebuild to avoid stale-archive skew
- Merge branch 'master' into codegen-split
- Merge branch 'codegen-split'
- Std/bytes + UInt8[] fs migration + flat-array free-layout fix
- Merge branch 'master' into lowlevel-bytes-stdlib
- Merge branch 'master' into worktree-agent-a6631e4a641df79b1

# Conflicts:
#	crates/lin-codegen/src/codegen.rs
- Merge nounwind codegen attrs + benchmark harness

- perf(codegen): mark user/closure functions nounwind (value-based errors
  never unwind; runtime lin_* decls left unmarked since runtime is panic=unwind)
- bench: end-to-end benchmark harness for compiled Lin + stale-runtime guard

Full gate passed: cargo build --workspace, cargo test --workspace (200),
stdlib suite (15/15), all CI examples.
- Merge branch 'master' into lowlevel-bytes-stdlib
- Merge branch 'master' into phase5-split

# Conflicts:
#	crates/lin-check/src/checker.rs
- Merge branch 'master' into phase5-split
- Merge branch 'phase5-split'
- Merge branch 'master' into json-cell-bugfix

# Conflicts:
#	crates/lin-codegen/src/codegen.rs
- Merge branch 'master' into json-cell-bugfix

# Conflicts:
#	crates/lin/tests/integration.rs
- Fix Json var-cell/global use-after-free + reassignment leak (symmetric owning RC model)
- Devcontainer memory
- Merge string-literal interning (immortal cache)

Intern compile-time string literals so each is allocated once instead of
per-evaluation. object_access -9%, no regressions. Full gate + ASan passed.
- Merge branch 'fix-flaky-tests': fix flaky HTTP test under parallel runs
- Merge branch 'master' into uint-display
- Merge branch 'master' into for-callback-leak
- Fix lin_for callback-return per-iteration leak

Anonymous closures use the uniform boxed return ABI; lin_for discarded the
returned TaggedVal* every iteration. Force a union ret_ty so codegen emits a
call ptr, then tag-aware release the box per iteration. Make the module-global
store path symmetric with the captured-cell path (clone-on-store) so the
released return is never an alias of the global's value.

Halves the per-iteration leak for callback-return-dominated bodies (~64->32
B/iter). Sibling per-iteration leaks (toString string temps, print-arg boxes,
tagged-array element boxes) remain and are tracked separately.
- Merge branch 'fold-orphan-tests': consolidate stdlib test coverage
- Merge branch 'master' into flat-unsigned-display
- Default argument values: parser + type checker (front-end)

Trailing-comma inversion for currying; Type::Function.required;
default-fill vs partial in infer_call/infer_dot_call; optional-last rule.
Backend (IR adapters + closure descriptor) not yet implemented.
- Default argument values: IR adapters + closure descriptor (backend)

- IR: synthesize per-arity default-fill adapters (f$default{k}) in the
  defining module; route static direct/dot/imported calls to them.
- Codegen: closure struct grows 32->40 bytes with a default-argument
  descriptor at offset 32; indirect under-arity calls through a function
  value dispatch via the descriptor. Runtime frees 40 bytes.
- Fix pre-existing boxed-ABI-wrapper bug: a function value returning a raw
  String/Array/Object crashed the indirect caller (which unboxes); the
  wrapper now boxes per the real Lin return type.
- Tests: 8 default-arg integration tests (basic, chained, object, indirect
  value, cross-module, trailing-comma currying, too-few error, optional-last
  error); examples/default_args.lin.
- Docs: SPEC 9.5/10.2/10.6, ADR-041.
- Merge feat/default-args: default argument values

Trailing-comma inversion for currying; per-arity default-fill adapters
(static + cross-module); closure descriptor for indirect default-fill.
Fixes a pre-existing boxed-ABI-wrapper bug for function values returning
raw String/Array/Object. See ADR-041 and SPEC 9.5/10.2/10.6.
- Merge branch 'master' into lowlevel-net
- Merge branch 'master' into for-element-box-leak
- Fix for/while per-iteration element-box leak

lower_for/lower_while handed each element to the callback as a fresh
TaggedVal* box (from lin_array_get_tagged or box_to_json) that was never
reclaimed — leaking the 16-byte shell every iteration (~36 B/iter). After the
callback call, reclaim the element box shell via FreeBoxShellIfDistinct (for)
/ FreeBoxShell (while). The _if_distinct guard skips the free when the callback
returned that very box (identity bodies, acc=f(acc,x)), which the loop's
return-box release already reclaimed. Scoped to for/while only — map/filter/
reduce keep the plain path so element-into-result moves are intact.

Flat for over 2M elements: 67MB -> 10.9MB peak. 183/183 integration, 18/18
stdlib. Remaining: nested inner-closure leak + toString/print box temporaries.
- Merge branch 'master' into lowlevel-layer4
- Fix call-argument boxing leak (concrete heap arg → Json param)

When a concrete heap value (Array/Object/Str) is passed to a Json/union
parameter, the lowerer boxes it into a fresh TaggedVal* shell that was never
freed — leaking one box per call (most visible in nested loops:
range(N).for(j => xs.for(...))). Json params are BORROWED (verified: callees
only register Function-typed params for release at exit), so the caller owns
the shell. After the call, free just the 16-byte shell via
FreeBoxShellIfDistinct (guarded against pass-through callees returning the
box). Inner payload untouched — released by the arg's own scope-exit.

Nested-loop probe: 73MB -> 11MB at 2M iters. 186/186 integration, 18/18 stdlib.
Residual ~3.6 B/iter is the separate inner-closure-allocation leak.
- Async design
- Merge branch 'master' into fix-array-closure-rc
- Merge branch 'master' into localset-box-leak
- Merge branch 'master' into compiler-bugfixes
- Merge branch 'master' into localset-box-leak

# Conflicts:
#	crates/lin/tests/integration.rs
- Fix LocalSet box-shell leak + structural deep array equality

Two fixes:
1. LocalSet store into a Json/union cell/global boxed a fresh concrete value
   then cloned it (for the slot + the result) without freeing the transient
   TaggedVal* shell — leaking ~36 B/iter for loops like `last = toString(i)`.
   Now frees the shell via FreeBoxShell after both clones (mirrors Var-init's
   coerce_and_own_store).
2. lin_array_eq compared heap/array elements by POINTER, so arrays of
   distinct-but-equal heap elements compared unequal. Now reads each element
   into a TaggedVal (handling tagged AND flat scalar arrays) and compares via
   lin_tagged_eq — deep structural equality. Fixes the previously-ignored
   test_array_equality_with_heap_elements; the flat-array handling also fixes
   nested arrays like [[1],[2,3]] reached by recursion.

192/192 integration, 18/18 stdlib.
- Merge branch 'master' into fix-array-closure-rc
- Merge branch 'master' into fix-concat-flat
- Merge branch 'master' into fix-array-closure-rc
- Merge fix-array-closure-rc: indirect-call arg boxing + Assertion[] test bodies

- fix: box arguments to Json params in indirect (closure-value) calls (ADR-042)
- test: require Assertion[] test bodies so every assertion runs; migrate all
  stdlib and example test files to the array form
- Raspberry-controller — deathbot RC-car client ported to Lin (Milestone 21 capstone)
- Merge branch 'master' into example-controller

# Conflicts:
#	docs/TODO.md
- **raspberry-controller**: Drop workarounds now that bugs are fixed
- Merge branch 'master' into example-controller

# Conflicts:
#	stdlib/number.test.lin
- Merge branch 'ci-wire-examples'
- Merge branch 'master' into simplify-test-bodies
- Merge simplify-test-bodies: bare assertion arrays + idiomatic dot-syntax

- feat(parse): line-leading [ / ( starts a new statement in inline bodies (ADR-043)
- test: drop 'val checks' wrapper, use bare assertion arrays, convert to
  first-argument dot-syntax across all stdlib and example test files
- docs: update std/test examples in STDLIB.md
- Merge branch 'master' into async-impl

# Conflicts:
#	crates/lin-runtime/src/array.rs
#	docs/DECISIONS.md
- Merge branch 'master' into async-impl
- Merge branch 'master' into async-impl
- Implement deathbot server modules in Lin (protocol, motor, NAL, RTP)
- Merge branch 'master' into deathbot-impl
- **deathbot**: Protocol parses to Float64 (de-workaround after float-widen fix)
- Track owned boxed-union call results for scope-exit release (json-call-result leak)
- Merge branch 'master' into json-call-result-leak
- Fix discarded Json call-result leak (register owned boxed-union results)

register_owned gated on is_rc_type, which excluded union/Json — so an owned
TaggedVal* returned by any Json-returning call (stdlib map/filter/reduce/
concat/keys/sort/...) was never scope-released when discarded or scope-bound,
leaking the box + contents (e.g. discarded map/filter in a loop grew RSS
unbounded). Now register_owned uses needs_owning (incl. unions), with
op-aware transfer_into_container (Push/object_set retain inner; tagged
array_set raw-copies = consume), FreeBoxShell for orphaned shells on raw-copy
pushes, and pop_scope dedup keeping exactly one owned ref per kept temp
(also fixes a pre-existing concrete-rc return-retain leak). Indirect-call and
if/match-phi merges deliberately stay is_rc_type-only (a branch/closure may
yield a borrowed union box; registering would double-free).

filter-in-loop leak fully eliminated (320MB->11MB@2M, flat across N).
map still scales ~31 B/iter from a SEPARATE pre-existing leak: stdlib map's
`var i` counter is a captured heap cell, and captured cells are never freed —
tracked separately. 200/200 integration, 18/18 stdlib, borrowed-return
canaries (minBy/maxBy) clean.
- Merge branch 'master' into deathbot-impl
- Merge branch 'master' into async-impl
- Remove now-dead call_thunk_value
- Merge async-impl: real OS-thread concurrency (ASYNC_DESIGN Phases 0-8)

Turns the synchronous async stub into real concurrency (spec §32):
- Phase 0-1: fault isolation — thunk faults unwind to the thread boundary and
  become Error values at await; nounwind dropped + uwtable under async (ADR-043).
- Phase 2: real async/await on std::thread; LinPromise (mutex+condvar);
  transfer-by-deep-copy (Option C) of thunk envs + results.
- Phase 3: order-preserving parallel; real race/timeout/retry.
- Phase 4: bounded ThreadPool (n workers + MPMC queue) via poolAsync.
- Phase 5: long-lived Worker (thread + mailbox): request/message/close + onShutdown.
- Phase 6: Shared<T> — atomic-RC box + RwLock (ADR-044).
- Phase 7: Frozen<T> — immortal deep-freeze, lock-free concurrent reads (ADR-045).
- Phase 8: TSan CI leg, stress tests, docs.

Also fixes a pre-existing lin_array_eq heap-overflow on flat scalar arrays.

Verified: full gate + ASan green; TSan green on the runtime race tests.
Known deferred (documented): Shared/Frozen compile-time type enforcement,
pool.async exact spelling (shipped as poolAsync), pool.serve, await
nested-promise flatten (§32.2.3), and T|Error / 'is Error' typing (§32.2.2).
- **concurrency**: Tour of all async/threading primitives + tests
- Merge async-gaps: close async spec gaps (§32.2.2 Error, §32.2.3 flatten)

- await auto-flattens nested promises (§32.2.3).
- Built-in Error type { type, message } + structural 'is Error' matching (§32.2.2);
  is <ObjectShape> now field-presence-checks instead of bare object-tag match.
- Removed dead call_thunk_value.

Deferred (ADR-046): Promise<T | Error> parametric typing so the checker rejects an
uninspected Error used as plain T — needs generic Promise<T>, its own project.

231 integration tests pass; ASan green. (The 2 RED examples — data-pipeline,
full-example — are a pre-existing master double-free from the json-call-result-leak
WIP, unrelated to async.)
- Merge fix-asan-ci: build host liblin_runtime.a so lin build's internal link succeeds in ASan leg
- Merge branch 'master' into async-example
- Merge async-example: concurrency tour example + tests

examples/concurrency/ — a small multi-file project illustrating every async/
threading primitive (async/await, parallel, race, timeout, retry, fault
isolation, nested flatten, ThreadPool, Worker, Shared<T>, Frozen<T>), with
tasks.test.lin + services.test.lin asserting every exported helper.

Verified: main.lin runs interpreted + as a native binary; 2 test files pass
(33 stdlib+example test files green total); both test programs ASan-clean.
- Merge test+docs hardening: process-unique fs temp paths + literal-cache comment
- Merge branch 'master' into fix-escaping-arg-box-uaf

# Conflicts:
#	crates/lin-ir/src/lower.rs
- **calc**: Expression interpreter (lexer + parser + evaluator) + tests
- Add concurrency benchmarks (async/await, parallel, pool, lock, worker)
- Merge async-bench: concurrency benchmarks

Adds 5 end-to-end benchmarks for the threading runtime (async/await round-trip,
parallel speedup, ThreadPool dispatch, Shared<T> lock contention, Worker
round-trip), picked up by benchmarks/run.sh, with README guidance on reading
latency-vs-throughput results. Benchmark .lin programs only — no compiler
changes.
- Merge branch 'master' into captured-cell-free
- Merge branch 'master' into captured-cell-free
- Merge branch 'master' into captured-cell-free

# Conflicts:
#	crates/lin-ir/src/lower.rs
- Free provably-non-escaping captured var cells at scope exit

A var mutably captured by a closure becomes a heap cell (MakeCell) that was
never freed — leaking the cell + its owned value on every creation. Most
visible in stdlib map's `var i` counter: a discarded map in a loop grew RSS
unbounded (~31 B/iter).

Adds a conservative escape analysis: a cell is freed (FreeCell) at the
creating function's scope exit ONLY if (1) created in the entry block (SSA
dominance) and (2) every capturing closure was lowered as a synchronous,
non-retained callback argument to a known std/array combinator (for/while/
map/filter/reduce/find/some/every). Any other use (returned, stored, bound,
async, indirect callee) marks the cell escaping → never freed (leak, but
sound — no use-after-free). FreeCell releases the cell's owned value before
freeing the allocation.

map-in-loop leak fixed (73MB->11MB @2M, flat across N). Escape safety verified
(returned mutating counter prints 1,2,3; stored escaping closure matches
master). 241/241 integration, 18/18 stdlib.
- Merge branch 'master' into fix-fnbody-dotchain
- Merge branch 'master' into rationalise-tests
- **report**: CSV->validated report generator + tests
- Merge perf(object): faster object-literal construction (~10% on object_access)
- Merge branch 'master' into rationalise-tests
- Add prefix logical-not operator !
- Merge branch 'master' into fix-eq-boxed-operand
- Merge branch 'master' into rationalise-tests
- **config**: JSON config loader with schema + defaults + tests
- Merge branch 'master' into rationalise-tests
- **codec**: Binary TLV protocol codec + bit twiddling + tests
- **ffi, processes**: FFI demo + subprocess pipeline projects
- **matrix, web-server, dijkstra**: Idiomatic dot-syntax + e2e tests; absorb template
- Merge branch 'master' into shared-type-enforce
- Merge shared-type-enforce: Shared<T> accessor-only compile-time enforcement

Adds Type::Shared(Box<Type>) and wires it through checker/IR/codegen so the only
operations on a Shared value are shared/get/set/withLock — any other op (push,
indexing, …) is a compile-time type error. Shared<T> is invariant and never
widens to Json, so the guard can't be lost. Runtime representation unchanged
(boxed TaggedVal*(TAG_SHARED)). ADR-044 follow-up #1.

Visible under lin build/lin run (which resolve imports). Full gate + ASan green;
new negative test (push(s,7) rejected) + positive (get's copy-out usable).
- Merge branch 'master' into rationalise-tests
- Add ffi/processes mains, dijkstra e2e + web-server template tests, gitignore
- Delete loose feature-demo files; docs point at projects
- Merge stdlib-derust: use index-assign/.for sugar instead of Rust FFI
- Merge branch 'master' into rationalise-tests
- Merge fromjson-and-json-cast: fromJson type-directed JSON decode + close Json->concrete cast hole (ADR-048/049)
- Merge branch 'master' into rationalise-tests
- Merge fromjson-and-json-cast: fromJson type-directed JSON decode + close Json->concrete cast hole (ADR-048/049)
- Merge branch 'master' into rationalise-tests
- **config**: Fold master's from-json.lin into config/decode + tests
- Merge fix-is-objecttype-soundness: is <ObjectType> checks required fields (ADR-050)
- Merge branch 'master' into rationalise-tests
- Merge fix-generic-type-aliases: bind type params when resolving generic alias bodies
- Type with named aliases instead of Json; add per-project READMEs
- Merge literal-types: singleton string-literal types (ADR-051)
- Merge branch 'master' into examples-proper-types
- Merge branch 'master' into serve-impl

# Conflicts:
#	examples/web-server/handlers.lin
#	examples/web-server/main.lin
#	examples/web-server/router.lin
- Merge fix-multiline-union: multi-line tagged-union type bodies (spec §18 form)
- Merge branch 'master' into serve-impl
- Merge fromjson-strlit: fromJson validates string-literal field values (ADR-052)
- Merge branch 'master' into imported-types

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into fix-map-closure

# Conflicts:
#	crates/lin-codegen/src/codegen/call.rs
#	crates/lin-ir/src/ir.rs
#	docs/DECISIONS.md
- Merge branch 'master' into fix-empty-obj-infer

# Conflicts:
#	crates/lin/tests/integration.rs
#	examples/web-server/handlers.lin
#	examples/web-server/router.lin
- Merge branch 'master' into fix-empty-obj-infer
- Merge is-deep-validation: deep type validation for is <ObjectType> (ADR-053)

# Conflicts:
#	docs/DECISIONS.md
- Merge generics Phase 0: monomorphized generic function values
- Merge branch 'master' into fix-map-closure

# Conflicts:
#	crates/lin/tests/integration.rs
#	docs/DECISIONS.md
- Merge spec-is-has-semantics: correct is/has object-type semantics (ADR-054)
- Merge branch 'master' into std-process

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into fix-map-closure

# Conflicts:
#	docs/DECISIONS.md
- Merge adr-backfill-v2: backfill ADRs 056-059, tick completed TODOs
- Merge branch 'master' into fix-map-closure

# Conflicts:
#	docs/DECISIONS.md
- Merge generics Phase 3.5: harden single-module monomorphization
- Merge branch 'master' into fix-map-closure
- Merge branch 'master' into std-time
- Merge branch 'master' into fix-map-closure
- **ffi**: Commit libmathlib.a, test FFI in Lin (no Rust), run in CI
- **processes**: Rework single-file demo into a real task-runner project
- **result**: Rework single-file demo into a form-validation pipeline
- Merge branch 'master' into examples-test-structure
- Update devcontainer
- Merge branch 'master' into examples-test-structure
- Merge pull request #2 from Lin-Language/fix-asan-stale-runtime

Fix ASan stale-runtime link failures + README positioning
- Merge remote-tracking branch 'origin/master'
- **devcontainer**: Install libzstd-dev for LLVM 22 linking
- Merge branch 'master' into examples-test-structure
- Merge branch 'master' into int-literal-fix
- Merge perf-intrinsics: append/prepend/groupBy runtime intrinsics (TODO #597, #600)

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into stdlib-doc-fixes
- Merge branch 'master' into worktree-agent-aa167fe2dcdba8e40
- Phase 4 cross-module generic instantiation (Steps A+B)
- Merge branch 'master' into worktree-agent-aa167fe2dcdba8e40

# Conflicts:
#	docs/DECISIONS.md
- Merge generics Phase 4: cross-module generic instantiation (infrastructure)

Generic functions defined in an IMPORTED module are now monomorphized in the
importing module's lowering (the importer holds the full TypedModule for each
import, so it can clone+specialize the generic body and re-home its free refs).
Covers cross-module identity and the higher-order map shape; specializations are
native/unboxed (id$Int32 = 'define i32 ...(i32)'). See ADR-063.

Verified by me on this worktree's own base (9d1dcc4):
- cross-module id$Int32 native, zero boxing, correct output; 4 cross-module
  generic integration tests pass.
- no-op invariant HOLDS: array_pipeline AND calc IR byte-identical between merge
  HEAD and pure-master built in the same worktree (an earlier 'violation' was a
  cross-checkout stale-build artifact, not real).
- integration 333/0 (3 clean runs; 1 flaky fs/http cleared on re-run);
  stdlib+examples 59 files pass; array_pipeline runtime neutral, output correct.

DEFERRED (follow-up): converting stdlib map/filter/reduce to generic — the
lin_for closure-callback ABI passes TaggedVal*, so per-element boxing survives
generic signatures. The array_pipeline boxing payoff needs callback-ABI
specialization, the next increment. stdlib stays Json-typed for now.
- Merge branch 'master' into fix-array-allocate-filled
- Merge fix-concat-uaf: concat retains copied elements, fix use-after-free (ADR-064)
- Merge generics Phase 4.5: element-type-aware flat array write path
- Merge branch 'master' into docs/site

# Conflicts:
#	.gitignore
- Update guidance around git stash
- Phase 4.5b: flow-type intermediate alloc element type for flat arrays
- Merge branch 'generics-stdlib' into worktree-agent-a11f2fc16c4dc44b7
- Phase 6-pre: bind T=Json for Json args + safe import monomorphization of unbound TypeVars (ADR-066)
- Merge branch 'generics-stdlib' into worktree-agent-a090861c3ed333683
- Phase 6: genericize array.lin accessors at/set/indexOf (ADR-067)

Type-safety win for the 3 direct-index accessors (tag-aware both ways, no new
alloc, representation-consistent for flat+tagged+Json). map and all builders
remain Json: their per-element value flows through the opaque closure-callback
ABI which boxes it, so a flat U[] reader would get garbage — blocked on Phase 5b
(unboxed closure ABI). Numerics need <T:Numeric> constraints Lin lacks;
for/while/iter take iterables not T[]. See ADR-067.
- Merge branch 'generics-stdlib' into worktree-agent-a9ce0a16073f8a3cc
- Phase 5b (partial): route map/filter/reduce through typed intrinsics + latent-bug fixes (ADR-068)

Perf-neutral at -O2 (the 1.64x was a debug-build artifact, corrected in the ADR),
but lands 3 real correctness fixes — curried-callback codegen (i=>()=>i returned
garbage), malformed loop phi back-edge, and flat-read-on-tagged-array corruption —
plus representation-safe flat-output intrinsic routing as the foundation for the
real zero-box win (blocked on generic map/filter/reduce + filter flow-narrowing,
Phase 6-round2). Verified: array_pipeline correct + neutral, curried callback
correct, integration 355/0, stdlib+examples 59/59, ASan-clean.
- Phase 6-round2: zero-per-element-box map/filter/reduce pipeline (~12x at -O2, ADR-069)

THE MILESTONE PAYOFF. Generic map/filter/reduce + a capture-less-lambda inliner
in lower_map/filter/reduce: a monomorphic combinator chain
range(0,n).map(...).filter(...).reduce(...) now compiles to fully unboxed flat
loops (flat get/push, native arith, scalar accumulator through the phi) — no
per-element boxing, no closure indirection.

Verified independently (interleaved release min-of-11): array_pipeline
169ms → 14ms (~12x at -O2), identical output 1892804906. Integration 357/0,
stdlib+examples 59/59, ASan-clean (incl. the inliner's unboxed-element RC).
Capturing lambdas + stored-fn callbacks correctly keep the closure path.

R1 (examples/report union narrowing) solved by idiomatic match helpers; R2
(sortBy/minBy Json combinators) kept on non-generic _mapJ/_filterJ/_reduceJ
helpers. Supporting fixes: cross-module specialization for imported modules'
own generic calls; repoint_call_native return re-coercion.
- Land zero-per-element-box map/filter/reduce pipeline (~12x at -O2, ADR-069)

The generics milestone payoff: generic map/filter/reduce + capture-less-lambda
inliner make a monomorphic combinator chain compile to fully unboxed flat loops.
Verified: array_pipeline 168ms→14ms (~12x at -O2, interleaved release), identical
output; integration 358/0; stdlib+examples 59/59; ASan-clean (incl. object-array
filter). Includes the borrowed-element retain fix (ADR-069 R2) preventing a
double-free when filter pushes a borrowed source element into a tagged result.

NOTE: the 'segfault' that previously parked this work was traced to STALE
.lin-cache contamination — the cache held std/array's pre-generic Json signature
for filter/map, so call sites boxed the array against the stale Json param type
while the generic body read it raw. With clean caches the code is correct. A
follow-up should make stdlib signature changes invalidate dependents' .lin-cache
(stdlib is include_str!'d, not part of the cache key) — a real latent cache bug,
independent of this work.
- Merge remote-tracking branch 'origin/master' into feat/await-error-enforcement
- Merge branch 'feat/await-error-enforcement'

# Conflicts:
#	docs/DECISIONS.md
- Merge ADR consolidation: DECISIONS.md 70→50, repoint cross-references
- Capture // comments on a side channel
- Add Stmt::span() accessor
- Preserve comments in the formatter (reverse ADR-040)
- Lin fmt: wire comment side channel + comment-preservation tests

format_source (cmd/fmt.rs) and the integration fmt() helper now grab the
lexer comment side channel and call Formatter::with_comments. Adds tests:
- test_fmt_preserves_leading_comments (top-level + in-block)
- test_fmt_preserves_trailing_comments (one canonical space)
- test_fmt_comments_idempotent (mixed fixture)
- test_fmt_corpus_idempotent_and_comments_preserved (formats every
  stdlib/*.lin + examples/**/*.lin twice: pass1==pass2 and // count
  unchanged).
- Flat else-if chains + preserve inline branch comments
- Merge remote-tracking branch 'origin/master' into feat-replace-mocking
- Merge branch 'master' into feat-replace-mocking
- Merge branch 'master' into worktree-agent-aa26d5141b07bd505
- Merge branch 'docs/spec-adr-reconcile'

# Conflicts:
#	docs/SPECIFICATION.md
- Merge std/yaml and std/jq stdlib modules
- Add cross-language comparison suite (Lin vs Go/Rust/Python/Node)
- Merge fix-check-imports: lin check resolves imports

Pre-existing bug: lin check called check_module directly without resolving
imports, silently passing import-dependent type errors. Now routes through
lin-compile's shared import-resolving front end (also surfaces warnings).
Verified: full suite green (flaky http aside), regression fails pre-fix.
- Merge fix-formatter-bare-lambda: never emit paren-less lambda outside arg position

Pre-existing bug: lin fmt stripped parens from single-untyped-param lambdas,
producing 'x => x' which is invalid outside argument position (ADR-007), so
formatter output no longer parsed. Now always parenthesises (round-trip safe).
Verified: full suite green (flaky http aside), regression fails pre-fix.
- Merge fix-parallel-promises: parallel() handles already-spawned promises

Pre-existing memory-safety bug: lin_parallel assumed every array element was
a thunk closure and read the closure layout (cap_desc@40) off each element;
a TAG_PROMISE element made cap_desc garbage -> misaligned deref / abort in
env_is_transferable (transfer.rs). Now dispatches on element tag: thunks spawn,
already-spawned promises are awaited and their value deep-copied (user keeps the
promise). Order preserved.

Verified: full suite green (flaky http aside), ASan clean (40/40 incl. the
promise-array double-free path), regression fails pre-fix with the abort.
- Add Go to comparison suite + provision Go in devcontainer; fix Lin Dijkstra data structures
- **dijkstra**: Scale graph to N=4000 (~33k edges); derive target from node count
- Merge templating: minijinja engine + layout system

Replaces the hand-rolled ${} substituter with minijinja and adds a file-based
layout system ({% extends %}/{% block %}/{% include %}) via path_loader.
See ADR-073 and ADR-074.
- **compare**: Scale recursion/parallel/pipeline so the real cross-language gap shows
- Merge feat/streams: Stream<T> + std/iter unification (code)
- Merge docs/streams: ADR-073/074/075 + SPEC + STDLIB (docs)

# Conflicts:
#	docs/DECISIONS.md
- Merge feat/streams-and-iter: Stream<T> + std/iter unified combinators

- Stream<T>: opaque affine resource, .drain()/.promise(), CAP_MOVE, fault isolation
- std/iter: unified combinators dispatched on receiver (eager arrays / lazy streams)
- Affine soundness fix: consume keyed off dispatch (closes promise/linesMax/close UAF holes)
- ADR-075/076/077, SPEC §18.7/§27.9, STDLIB + docs-site (std/iter, std/stream pages)
- Migrate combinator imports to std/iter (post-merge)
- Merge perf-inline-flat-reads: inline flat scalar-array reads (~3.6x dijkstra)
- **compare**: Add interp — a second generalized (real-algorithm) benchmark
- Merge feat/compress: std/compress streaming codecs + std/archive tar splitting

- std/compress: gunzip/gzip/inflate/deflate lazy streaming byte-adapters (flate2).
- writeStream is now a RAW sink (binary-safe); new writeLines for line-delimited output.
- std/archive: untar((meta,data)=>…) terminal with per-entry constant-memory sub-streams,
  manifest() listing, files() buffered entries. Sub-stream is sync-only (documented).
- examples/streams extended with a multi-file tar.gz extract + manifest demo.
- docs-site pages for std/compress and std/archive; STDLIB/SPEC/DECISIONS updated.

Verified: cargo test --workspace green; lin test stdlib/ examples/ 67/67; example
runs end-to-end (byte-identical extraction); ASan clean on stream/archive paths.
- Merge branch 'master' into fix/parens-block
- Merge perf-single-alloc-obj: single-allocation LinObject (header+entries in one block)
- **report**: Read CSV input from a streaming .tar.gz; revert streams demo
- Merge perf-construct: drop dead string_release on immortal interned object keys

Runtime-neutral hygiene (the immortal-check release is a predicted no-op
call, not a real cost) but removes ~4 emitted calls per object-literal IR
and trims the construction/field-read/match-test code paths. Sound:
compile_string_lit keys are immortal (refcount == IMMORTAL_RC), so neither
the release nor object_set*'s key inc_ref ever changes a refcount.
- Merge branch 'master' into fix/match-parens
- Merge branch 'fix/match-parens'
- Stop the formatter silently miscompiling (parens + generics)
- Canonicalize stdlib/examples/benchmarks; gate fmt in CI
- Layout rules — chain threshold, blank-line preservation, no trailing commas, recursive JSON, arg/lambda layout, comment hoist
- Re-sweep stdlib/examples/benchmarks under new layout rules
- Reject trailing commas in array/object literals (Rule 3)
- Respect author newline choice for if/function-body/2-chain (Rules A/B/C)
- Re-sweep stdlib/examples/benchmarks under author-newline rules (A/B/C)
- Merge master into feat/fmt-sweep; resolve fmt + re-sweep under author-layout rules

Master advanced ~30 commits (streams, jq/yaml stdlib, benchmark comparison
suite, and d6e7bdb "never emit paren-less lambda outside arg position").

Conflict resolution:
- formatter.rs: took master's always-parenthesise-lambda-params rule (round-trip
  safe; bare `x =>` is only legal in argument position, ADR-007) AND kept this
  branch's generic `<T>` emission — `(x) => …` and `<T>(…) => …` both correct.
- examples/config/main.lin, stdlib/array.test.lin: took master's pristine layout;
  re-swept below.

Re-swept all stdlib/examples/benchmarks from the merged tree under the
author-layout rules (preserve author newline choice for if / function body /
2-chain; >2-chain always multiline; recursive JSON; no literal trailing commas).
Master's new files contained literal trailing commas the branch parser now
rejects, so added LIN_FMT_ALLOW_TRAILING_COMMA — an escape hatch letting `lin fmt`
parse legacy files to strip their trailing commas; normal compilation still
rejects them.

Updated 5 formatter test expectations to the parenthesised-lambda form.

Verified: 63/63 stdlib+examples, 26/26 fmt tests (incl. corpus type-check gate),
fmt --check clean, workspace green (modulo the known-flaky localhost http test).
- Preserve radix-prefixed integer literals (0x/0b/0o)
- Test-suite array rules — close-paren on own line, array-element comments, blank between tests
- Re-sweep stdlib/examples/benchmarks under test-suite array rules (i/ii/iii)
- Re-sweep from master originals — fix stale comment-migration corruption
- Merge branch 'master' into feat/fmt-sweep

# Conflicts:
#	benchmarks/compare/dijkstra/dijkstra.lin
#	examples/web-server/handlers.lin
#	examples/web-server/handlers.test.lin
#	examples/web-server/integration.test.lin
#	examples/web-server/template.test.lin
#	stdlib/array.lin
#	stdlib/array.test.lin
#	stdlib/template.lin
#	stdlib/template.test.lin
- Over-budget test(name, () => [...]) breaks the body, not the arg list
- Re-sweep stdlib/examples/benchmarks (over-budget test-lambda layout)
- Multi-call arrays multiline, bare arg-lambdas, preserve author parens
- Re-sweep stdlib/examples/benchmarks (array/lambda/paren fixes)
- Respect author layout — implicit else null, multiline literals, close-paren glue
- Re-sweep (author-layout: else-null, multiline literals, close-paren)
- Preserve author lambda-body newline on inline fast-paths
- Re-sweep (preserve author lambda-body newline)
- Merge remote-tracking branch 'origin/master' into feat/fmt-sweep

# Conflicts:
#	stdlib/stream.lin
#	stdlib/stream.test.lin
- Re-sweep on latest master (compress/archive/parens-block); fix trailing-comma test
- Keep trailing comment on a single-line array element; omit else null on block tail
- Merge remote-tracking branch 'origin/master' into feat/fmt-sweep

# Conflicts:
#	examples/streams/main.test.lin
- Re-sweep on latest master (single-alloc LinObject, report tar.gz demo)
- Respect match-arm body newline; refine array-multiline to >1 call; keep element/val-array comments
- Merge feat/fmt-sweep: comment-preserving formatter + full layout rules

Reverses ADR-040 (formatter now preserves // comments), hardens the formatter
against silent miscompiles (generics, grouping parens, postfix-base parens), and
adds the full layout ruleset: chain threshold, author-newline preservation for
if/function-body/2-chain/match-arm bodies, blank-line preservation, recursive JSON,
no trailing commas (parse error), arg-list/lambda layout, opt-in column alignment
for match arms and trailing comments, and a comment+run-equivalence gate.
Swept stdlib/examples/benchmarks; CI runs lin fmt --check.
- Add Format command + DocumentFormattingEditProvider
- Vscode Format command + DocumentFormattingEditProvider
- Merge feat/static-obj-construct: inline scalar-only object literal construction

Phase 1 of static-record layout. No-spread object literals whose fields
are all concrete scalars get inline LinObject entry stores instead of
per-field lin_object_set_fresh calls. +6.7-7.0% on a 20M-object scalar
record construction microbench; RC-trivial (scalars need no retain,
immortal keys no inc_ref, alloc(N) means no grow so entries never move);
LinObject byte-identical so all consumers unchanged. ASan-clean across
read/eq/spread/nested/toString/keys + churn + calc/config examples.
- Phase B: raise stdlib test coverage (tty, signal, net TCP, io, process, http, bytes)

Add colocated tests for the two previously-untested modules (tty, signal) and
raise coverage on net/io/process/http/bytes. Syscall-bound functions are tested
only on safe, deterministic paths; blocking signal.waitSignal is intentionally not
exercised live (covered by a Rust unit test). Test files only.
- Hash is generic over its input type — hash = <T>(x: T): String
- Phase C: hash is generic over its input type (<T> instead of Json)
- Merge feat/static-obj-rc: inline object construction for RC-typed fields (Phase 2)

Extends inline object-literal construction from scalar-only to concrete heap
fields (Str/Array/Object), with one lin_rc_retain per heap field mirroring
the runtime set_fresh->retain_tagged_payload exactly. Function fields excluded
(retain is a no-op for TAG_FUNCTION) and fall back to the runtime path.
+7.6-9.3% on a 10M-object RC-field construction bench; ASan-clean across
shared-reference build/free churn + full example corpus; LinObject byte-
identical so all consumers unchanged.
- Phase F: ADR-078 — reject Rust->Lin stdlib migration on pilot evidence
- Revert "Phase F: ADR-078 — reject Rust->Lin stdlib migration on pilot evidence"

This reverts commit 46f228b145959c0b79ae87c8f85f30a1541ae689, reversing
changes made to a8d343bd355b25e0fce766404ce9a6484918aec8.
- Fix int-element widening in mixed flat array literals
- JSON test reporter, toJson serializer, and VSCode Test Explorer
- Merge perf/string-charcode: O(1) byteAt string primitive (fixes O(n²) Lin-side scanning)

charCode/codePointAt are O(n) per call (chars().nth walks UTF-8 from the start),
making a pure-Lin loop over 0..length(s) O(n²) — the real cause of the ~6-10x
self-host stdlib slowdown. Adds lin_string_byte_at: an O(1) raw UTF-8 byte load
(same byte-index space as the existing char_at), exposed as std/string.byteAt.
codePointAt/charCode left codepoint-correct and O(n). Path pilot: worst-case
indexOf 304x→10.6x vs native; short-string 2.7x. ASan-clean at all boundaries;
460 cargo + 68 runtime + 69 lin test green.
- Merge perf/byteat-inline: inline lin_string_byte_at (+20%)

byteAt is a hot O(1) per-byte accessor in Lin-side string scanning; intercept
the direct 2-arg call in codegen and emit the bounds-checked indexed load inline
(flat-array-read treatment, ADR-069) instead of a non-inlinable staticlib call.
+20% on a byteAt scan bench. ASan-clean at all boundaries; first-class/partial
byteAt falls through to the runtime fn. 463 cargo + string tests green.
- Structured expected/actual in test diffs + cancellable palette runs
- Unify formatter into single comment-preserving format_source
- LSP completion from imports, test output recording, portable coverage link
- Substring negative indices + optional end
- Number as a zero-cost numerically-bounded generic type
- Devcontainer lock, remove old docs
- Merge fix/lsp-cyclic-import-guard: cycle guard + stdlib list sync
- Merge fix/runtime-ffi-panic-safety: panic-safety at FFI boundaries
- Merge fix/parser-hang-formatter-comma: parser hang guard + formatter partial-app comma
- Merge fix/rc-elide-liveness: sound RC elision (post-dominance + liveness gate)
- Merge fix/codegen-tags-uint64: shared tag constants + Float32/64 tag + UInt64 signedness fixes
- Merge fix/cache-key-import-sigs: import-signature-aware cache key + version stamp
- Merge fix/codec-example-uint64: codec length prefix via push-coercion
- Merge branch 'master' into worktree-agent-a1568fdf4b5530e42
- Merge master: drop now-redundant lin-lsp std/ffi (already on master)
- Merge fmt fix: canonicalize docs-site/builder/markdown.lin (CI fmt --check drift)
- Merge branch 'master' into worktree-agent-ad28432b684a17eb9
- Merge stream-iter fix: pass trailing index arg to combinator callbacks (fixes macOS/arm64 misaligned-pointer abort)
- Merge branch 'master' into worktree-agent-a51c8dc8caf6ee194
- Merge macOS rpath support: @loader_path token + install_name fixup for vendored dylibs; cross-platform relocatability test (ADR-080)
- Merge perf/flat-array-write-inline: inline flat-scalar array PUSH/SET in codegen

~1.65x on the pipeline benchmark (109ms -> 66ms, now beats Rust 88 / Go 109);
no regression on the other 5 workloads. Mirrors the proven flat_array_get
read inline; cold grow/OOB paths defer to the runtime so behaviour is
byte-identical. RC-trivial (flat scalars carry no refcount). 493/493
integration tests pass; ASan-clean across grow paths, int/float, map/filter.
- Merge perf/union-discrimination: cheap StrLit discriminator for closed-concrete-union `is V`

When a match/is scrutinee's static type is a closed concrete union (every
non-Null variant a concrete object, no TypeVar/Json) discriminated by a StrLit
field, compile `is V` to a single field-read + value-compare instead of the
recursive lin_matches_schema validation. Removes the ~40% penalty that made
concrete-typed unions slower than :Json (up to ~10x on match-dominated code);
zero change on the benchmark suite (none use the eligible pattern). Field-
presence is deliberately NOT a discriminator (width subtyping makes it unsound);
those cases keep full MatchesSchema. 499 integration tests pass, ASan-clean.

Enabling change for the 'precise types are the fast path' direction (pairs with
future sealed/literal-discriminant records).
- Stage 1 sealed scalar-record struct layout (checkpoint, gates not yet run)
- Add RAPTOR journey-planner port + cross-language benchmark

Port of planarnetwork/raptor (GTFS journey planner) to Lin, Go, Rust,
Python and Node.js under benchmarks/compare/raptor/. Node is the golden
reference (faithful type-stripped copy of the TS); the other four are
checked against the same ~40 unit tests it passes.

- Full journey planner + transfer-pattern subsystem, all unit tests green
  (node 48, go 48, rust 51, python 51, lin 9 files / 48 cases).
- GTFS loader + CLI runner per language; all five plan TBW->NRW over the
  real UK rail feed (gtfs.tar.gz) with byte-identical RESULT lines.
- Complex-query benchmark (bench.sh + per-language bench): 24 group-station
  queries + next-20 range queries, load/query timing split, order-independent
  correctness digest agreed by all languages.
- LIN_ISSUES.md: six Lin language/stdlib problems found while porting, each
  with a minimal repro (two correctness bugs, an Int64 widening footgun, the
  O(n) Json-map / missing stable-sort perf gap, a Json+null semantics gap).

Lin notes: a stable merge sort replaced an O(n^2) insertion sort that made
the full feed appear infeasible; loader streams the CSV; scanTransfers guards
missing keys to match JS NaN-skip semantics.
- Merge branch 'master' into docs/sealed-records-design
- Merge fix/lin-closure-var-if: closure-local var written in if persists past join
- Merge branch 'master' into docs/sealed-records-design
- **records**: Add record-access-bound cross-language workload + fix sealed-record TCO self-call
- Merge fix/lin-imported-var-mutate: imported-module top-level var mutator
- Merge fix/lin-int-widening: mixed Int32*Int64 widens the Int32 operand
- Merge fix/lin-json-map-semantics: dynamic Json arithmetic faults on non-numeric operand (#5) + hashed-object proposal (#4b)
- Merge fix/lin-ergonomics: actionable ';' diagnostic + CLI args regression coverage

# Conflicts:
#	crates/lin/tests/integration.rs
- **raptor/lin**: Use stable std/array.sort; drop workarounds for fixed bugs
- Merge fix/lin-if-else-in-parens: parse wrapped if/else inside parens (LIN_ISSUES #7)
- **raptor/lin**: Remove bug-era workarounds now the language fixes landed
- Merge branch 'master' into docs/sealed-records-design (pre-merge sync)

# Conflicts:
#	crates/lin/tests/integration.rs
- Merge sealed-records Stages 0.5–2: unboxed struct layout for named records

Named record types (type T = {...}) now get an unboxed, constant-offset
struct representation instead of the boxed string-keyed LinObject, while
type COMPATIBILITY stays structural (a wider/Json value projects into a
fresh sealed copy at the boundary, non-mutating).

- Stage 0.5 (c061ec1): inert  marker on Type::Object carried
  through resolution; manual PartialEq ignores it so inference/narrowing/
  compat are unchanged. Cache format bumped 1→2.
- Stage 1 (d15f5c2): all-scalar sealed records → packed heap struct; ~83x
  faster field access vs boxed on an access-bound loop.
- Stage 2 (44969a6): heap-field records (String/Array/nested) via a per-type
  field descriptor in the struct header driving per-field RC at every drop
  site; ~5.9x vs boxed; ASan-clean.
- records benchmark (424ddde): record-access cross-language workload; ~5.7x
  sealed-vs-Json in Lin. Surfaced + fixed a TCO self-call heap-corruption bug.

std/test migrated (Assertion now returns Json) — the one corpus type that
used a named type as an open extra-field carrier (the §2.2.2 lossy-projection
semantic). Arrays of sealed records (Stage 3) and stack alloc (Stage 4) are
follow-ups. Design reference: docs/SEALED_RECORDS_DESIGN.md (to become an ADR +
spec edits, then deleted). 532 tests + 71 corpus green, ASan-clean.
- Merge master into feat/sealed-record-arrays (pre-merge sync)
- Merge sealed-records Stage 3: contiguous unboxed arrays of scalar records

A MyType[] of an all-scalar sealed record is now a LinArray of contiguous,
header-less packed element payloads (new elem_tag 0xFE; element stride +
field descriptor in trailing LinArray fields, leaving flat/tagged offsets
0/4/8/16/24 untouched). The array owns its elements, so there is NO
per-element refcount; arr[i].field is a constant-stride GEP+load (fused
SealedArrayFieldGet), not lin_array_get + lin_object_get per element.

Measured ~87x vs a boxed Json[] on an access-bound field-sum loop;
ASan-clean across build/index/drop cycles. Scalar-element only — arrays of
heap-field records (Person[]) stay boxed (Stage 3b, plumbing in place but
deferred pending its own ASan campaign). Fast: index-read/construct/length/
push/index-set/drop; boxed-fallback: Json boundary, ==, toString, combinators.

539 tests + 71 corpus green. Two RC bugs (materialized-arg leak, owned-
registration double-free) found and fixed under ASan during development.
Stage 4 (stack-alloc non-escaping records) is the next target.
- Merge branch 'master' into fix/lin-hashed-object
- Merge fix/lin-hashed-object: lazy O(1) hash side-index for large Json objects (RAPTOR #4b)

Large Json objects (>=16 entries) gain a lazily-built open-addressing hash
side-index for O(1)-average key lookup, removing the O(n^2) dictionary wall.
Assoc-list entries stay the source of truth; index fields appended at offset
>=24 so the codegen MakeObject ABI is untouched. Index stores only u32 slots
(no refcounted pointers), every probe hit reconfirmed by key-eq.

- RAPTOR PREP index-build ~145s -> ~27s; cross-language digest unchanged
  (group=26203913 range=773022892 journeys=139)
- microbench 16k-key build 142ms -> 0.7ms (O(n^2) -> O(n))
- fuzz/oracle test vs linear scan across the threshold, ASan-clean
- ADR-081 records the design; implemented proposals retired
- Merge fix/stdlib-collection-generics: Category 1 — element generics for std/array + std/iter
- **raptor**: Introduce named record + union types for fixed-shape data
- Merge cleanup/raptor-typed-records: named record + union types for the RAPTOR Lin port
- Ignore tmp
- Merge branch 'master' into worktree-agent-a3f5ae071b012b4eb

# Conflicts:
#	crates/lin-codegen/src/codegen/boxing.rs
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-a3f5ae071b012b4eb
- Merge branch 'worktree-agent-a3f5ae071b012b4eb'

# Conflicts:
#	docs/proposals/stdlib-json-typing-audit.md
- Merge master into docs/sealed-records-adr (resolve ADR number collision)
- Merge ADR-083 sealed records + spec 5.9.1; retire design doc

Promote the shipped sealed-records work (Stages 0.5/1/2/3) into the permanent
record: ADR-083 in DECISIONS.md, SPECIFICATION 5.9.1 (sealed named records +
lossy projection + the open-carrier semantic change), Error noted as not-sealed.
Delete docs/SEALED_RECORDS_DESIGN.md; code-comment refs repointed. Renumbered
082->083 to avoid collision with the index-signature ADR-082 that landed on
master concurrently.
- Merge sealed-records Stage 4: stack-alloc + RC-emission suppression

Non-escaping all-scalar sealed records are stack-allocated, and — the key that
makes it a win — Retain/Release EMISSION is suppressed in lowering for values
the escape analysis proves stack-resident, so the no-op RC calls vanish from
the IR and the entry-block alloca SROA-promotes to registers.

lin_ir::escape (recovered + extended from the earlier Stage-4 prototype) is a
sound escape analysis: carry-class union-find over representation-preserving
aliasing + self-tail-call arg<->param unification, fail-safe to heap on any
Return / container-store / closure-capture / repr-changing-coerce / unknown
retaining call. It marks non-escaping all-scalar sealed MakeObject stack=true
and deletes Retain/Release on the proven-stack-resident class.

records @step hot loop: 25 retain / 25 release / heap-alloc per iter -> 0/0/
reused alloca. Measured ~5.1x (mine) / ~6.1x (agent) vs current master, both
clean release runtimes interleaved — records now BEATS Go. ASan-clean over the
full corpus + escaping fixtures (returned/array-stored/closure-captured stay
heap, no use-after-return; high-N TCO no stack growth). 555 tests + 72 corpus
green. ADR-083 updated (stack-alloc reframed rejected->shipped). Heap-field
records stay heap (scope = all-scalar).
- Merge fix/nested-map-codegen: dispatch TAG_MAP on nested-map union index read/write
- Merge cleanup/raptor-map-types: type RAPTOR createRaptor index maps as { String: T } (O(1))
- Merge fix/stdlib-object-maptype: Category 2 — type std/object map producers + groupBy/countBy over { String: T }
- Merge feat/null-complement-narrowing: == null / is Null narrows T|Null to T in the complement branch
- Merge feat/defaulted-accessors: object.get + array.atOr defaulted accessors; RAPTOR cleanup; lin-ir Map monomorphization
- Merge fix/is-generic-typevar: substitute is-pattern target type in monomorphization (silent-wrong-result fix)
- Inline match in map callbacks; retire obsolete ADR-004/014 workaround
- Fix empty array literal arg adopting flat-scalar param element type
- Fix empty object literal arg adopting Map param value type
- Merge feat/generic-push: generic push/append/prepend <T> + require annotation on empty literals

Closes the soundness hole where push(intArr, "str") type-checked. Bundles Phase 1
(empty-literal annotation requirement, ADR-084) — a hard prerequisite, since without it
an untyped [] is Never[] and generic push mis-stores. Fixes 4 representation bugs the
generic signature surfaced (literal-width clobber across args, concrete-object element
materialization, cross-module nested generic push inlining, Json-into-flat dynamic
dispatch). RC verified under ASan (no UAF/double-free; leak count identical to master)
+ debug underflow guard clean on 46k-push churn. ADR-085.
- Merge feat/accessor-t-or-d: unify at/get to <T,D>(...): T|D (default's type independent of element)
- Merge chore/stdlib-typing-audit: type number/time/tty/env Json returns as their real T|Null / T|Error
- Merge fix/raptor-empty-literal-annotations: conform RAPTOR to empty-literal rule + generic push
- Merge feat/union-complement-narrowing: is X narrows the complement branch (generalizes Null-only narrowing)
- Merge branch 'master' into feat/generic-array-callbacks
- Merge feat/generic-array-callbacks: generic sort/sortBy/minBy/maxBy <T> callbacks

Closes the loose-Json callback hole on the remaining std/array sorting/selection fns:
sort = <T>(arr: T[], cmp: (T,T)=>Int32): T[], sortBy = <T>(arr: T[], keyFn: (T)=>Json): T[]
(Json-key fallback — heterogeneous pair array can't carry a 2nd type param), minBy/maxBy
= <T>(arr: T[], keyFn: (T)=>Number): T. Internal _merge* helpers genericized; buffers use
arrayAllocateFilled (flat T[] repr) to keep the monomorphized comparator reading packed
scalars. Mistyped comparators (wrong return type / explicit param type) and the element
type now flow through; the residual gap — T not back-inferred into UNANNOTATED comparator
params — is a pre-existing general inference limitation, tracked separately. 594 tests,
RAPTOR 9/9, fmt clean.
- Update lin engineer
- Merge fix/json-to-map-widening: reject unsound Json -> { String: T } coercion in trusted-stdlib path
- Merge fix/await-benchmark-narrowing: narrow await's T|Error in thread_pool/async_await benchmarks
- Merge fix/sealed-scalar-array-push: heap-corruption fix for record push into sealed scalar array

push(arr, {...}) where arr is a sealed all-scalar-record array (Pt[]) wrote a
boxed LinObject element (TAG_OBJECT, pointer-sized) into the contiguous
packed-scalar-stride buffer via lin_array_push → heap-buffer-overflow /
realloc abort. Stage-3 array tests missed it (literals only, never push).

Fix (codegen-only, intrinsics.rs Intrinsic::Push): when the receiver is a
sealed scalar-record array (existing sealed_array_elem gate), route to the
existing lin_sealed_array_push_struct_retaining (contiguous payload copy +
balanced RC) instead of the boxed-materialize tagged push. Heap-field record
arrays stay boxed (unchanged). 600 tests + 72 corpus green; ASan confirms the
overflow is gone, no new leak/UAF. 6 regression tests added.
- **dijkstra**: Rewrite to idiomatic typed index-signature maps (ADR-082)
- Merge fix/dijkstra-typed-map: rewrite dijkstra example to idiomatic typed index-signature maps (ADR-082), removing lin_object_set from user code
- Merge worktree-agent-a1112acbc79a0a331: enforce lin_* intrinsics are stdlib-only (ADR-086)

Gates lin_* compiler-intrinsic name resolution on Checker.allow_intrinsics
(set from is_stdlib + LIN_ALLOW_INTRINSICS test escape hatch) in infer_ident.
User code calling an intrinsic (e.g. lin_object_set) is now a hard type error
pointing at the stdlib equivalent. The dijkstra re-fix from this branch was
dropped in favour of master's existing typed-index-signature-map version.
- Merge branch 'master' into cleanup/raptor-scanstate-maps
- Merge cleanup/raptor-scanstate-maps: type RAPTOR bestArrivals/kArrivals as { String: Int64 }

Fidelity improvement (GROUP-neutral within noise — ~3k-key query-phase maps, not the
16k-key per-trip-scanned PREP indexes). bestArrivals -> { String: Int64 }, kArrivals ->
{ String: { String: Int64 } } via a typed ScanResults record. kConnections stays Json
(union-valued; a codegen/RC fault when typed is claimed but not yet reproduced — repro
to follow). 9/9 unit tests, run.lin gate exact, GROUP digest 26203913 unchanged.
- Remove projection-aliasing-uaf proposal (discussing inline instead)
- Fix projection use-after-free (val x = obj[k] held across container grow)
- Merge perf/scalar-sort-unboxed: inline unboxed scalar merge sort for capture-less literal comparators
- Merge ci/release-plz-automation: automated semver releases via release-plz (ADR-087)

- single workspace version → 1.0.0 (version.workspace across all 10 crates)
- release-plz.toml: release-PR flow, no crates.io, tag v{version}, grouped changelog
- reusable build-binaries.yml; release.yml rewired (rolling latest retained); release-plz.yml
- docs-site /changelog page + nav + docs.yml mirror
- install.sh installs newest stable release by default
- Fold streams/wordcount/indexed into report + fix stream failed-source Error handling
- Merge fix/typed-map-nested-field: widen record literal to { String: T } in nested field position; type http headers
- Merge branch 'master' into chore/rationalise-adrs

# Conflicts:
#	docs/SPECIFICATION.md
- Type the last Json dictionary (kConnections : { String: { String: Conn } })
- Merge cleanup/raptor-kconnections: type the last Json dictionary (kConnections)

kConnections : { String: { String: Conn } }, Conn = [Json, Int32, Int32] | Transfer
(tuple head stays Json — composite Json doesn't flow into the named Trip record).
The projection-aliasing UAF that previously blocked this is fixed, so it types cleanly.
Consumers converted from isTransfer boolean guards to match … is Transfer
(journeyFactory/graphResults/stringResults/run). Fidelity win (query-phase maps measured
perf-neutral); RAPTOR is now Json-dictionary-free except the createRaptor inputs +
getQueue return. 9/9 unit tests, run.lin gate exact, GROUP digest 26203913 unchanged.

NOTE: surfaced a separate codegen bug — a nested typed map passed through a Function-VALUE
call loses inner entries (Function type erases the map type). Worked around in query.lin by
calling completeJourneys directly instead of via a fn param. To be fixed separately.

# Conflicts:
#	benchmarks/compare/raptor/lin/scanResults.lin
#	benchmarks/compare/raptor/lin/types.lin
- Merge fix/generic-callback-param-inference: back-infer resolved generic T into unannotated callback params
- Merge fix/map-unbox-closure-abi: unbox Type::Map in closure-ABI wrapper
- Merge master into feat/sealed-combinators-3b (pre-merge sync)
- Merge branch 'master' into worktree-raptor-bench
- Sealed-records Stage 3b: heap-field element-array machinery wired + gate (kept scalar-only) + scalar index-set-in-callee fix

Investigated ungating arrays of HEAP-FIELD sealed records (String/Array/nested-sealed element
fields) to contiguous unboxed element buffers. Wired the full per-element-per-field RC lifecycle
through every transition and proved each ASan-error-clean on hand-written fixtures — but the corpus
ungate hit TWO language-level blockers, so the gate is kept SCALAR-ONLY (Stage 3a), fail-safe to
boxed Object[] for heap-field element arrays. Corpus stays 72/72, cargo test --workspace green,
fmt clean, dijkstra/calc/report ASan-clean.

Single-source-of-truth gate predicate `Codegen::sealed_array_elem_field_packable`, mirrored EXACTLY
by lower.rs `is_sealed_array_elem_field_packable` and monomorphize.rs `field_packed_scalar` (the gate
is multi-site). Flip a heap kind to `true` in all three to re-attempt the ungate.

Heap-field RC machinery wired (inert under the scalar-only gate via `has_heap`/`is_heap`/`needs_proj`
compile-time guards, so scalar codegen is byte-identical; correct when a kind is enabled):
- sealed_array_materialize_elem (whole-element read `val x = arr[i]`): retain each heap field after
  the payload memcpy (the fresh struct is a +1 owner; was +0 borrowed → would double-free on drop).
- sealed_array_elem_materializer (Json-boundary thunk): per-field RC for heap fields — free only the
  box SHELL for String/Array (borrowed inner), full tagged_release for nested-sealed (fresh inner).
  Mirrors sealed_materialize_to_object.
- MakeArray literal construct: retaining push (`lin_sealed_array_push_struct_retaining`) for a
  heap-field element array so the array owns its own +1 per heap field.
- IndexSet into a sealed array: project a representation-mismatched RHS into a fresh sealed struct
  before lin_sealed_array_set, releasing it after the set's retained copy.

FIX (reachable + tested, benefits the SCALAR Stage-3a path): `arr[i] = { .. }` over a scalar sealed
array INSIDE A CALLEE typed the RHS literal as an unsealed boxed object and passed it straight to
lin_sealed_array_set, which memcpy'd garbage from `box + SEALED_HEADER`. Now projected first.
New regression test test_sealed_array_index_set_in_callee.

STAGE 3b BLOCKERS (documented at the gate; need a LANGUAGE change, not codegen):
1. FIELD OMISSION — structural typing lets a literal omit a declared field (calc's
   `{ "kind": "lparen" }` as Token{kind,text}). Boxed reads missing key → Null; a packed element
   stores a NULL String pointer and a later read derefs it (string.rs:68, the 0x7 crash). The packed
   repr is strictly less capable, and omission is whole-program (the element TYPE can't gate it).
2. CONTAINER ROUND-TRIPS — dijkstra (no omission) still crashes: a Neighbor[] packed array stored
   into a boxed {String: Neighbor[]} map slot and read back, plus push of a packed struct into a
   boxed-read array — a boxed-vs-packed mismatch across the container boundary the one-level
   sealed-array Coerce gates don't cover.
Full ungate measured 72→61; String-only 72→63. Both sound on the boxed path; kept boxed.
- Merge branch 'master' into worktree-raptor-bench
- Merge master into feat/sealed-heap-3b (pre-merge sync)
- Merge sealed-records Stage 3b machinery + scalar index-set-in-callee fix

Stage 3b (contiguous unboxed arrays of HEAP-FIELD sealed records) is wired at
every lifecycle site (construct/whole-element-read/field-read/index-set/drop/
transfer + Json-boundary materialize) but the gate stays SCALAR-ONLY: a single
source-of-truth predicate sealed_array_elem_field_packable (types.rs), mirrored
in lower.rs + monomorphize.rs. The heap-field RC machinery is inert under the
scalar gate (compile-time guards keep scalar codegen byte-identical).

Heap-field arrays remain BOXED because a full ungate is blocked by LANGUAGE
semantics, not codegen (verified across 4 attempts): (1) field omission — push()
permits a record missing a required non-null field; a packed slot stores NULL
and crashes on read (boxed tolerates missing->Null); (2) container round-trips —
a packed array stored into a boxed map value and read back is a boxed-vs-packed
mismatch. Both need exact/sealed records forbidding omission, an open language
decision. Flip a kind to packable in all three predicates to re-attempt.

Also fixes a genuine SCALAR-path bug: arr[i] = {...} over a scalar sealed array
in a callee (RHS typed unsealed->boxed) memcpy'd garbage into the slot; now
projected first (test_sealed_array_index_set_in_callee). 575 tests + 72 corpus
green, ASan-clean (incl. dijkstra).
- Merge branch 'master' into worktree-raptor-bench
- Merge master into fix/record-field-omission (pre-merge sync)
- Merge fix/record-field-omission: close field-omission soundness hole

A value of a named record type must have ALL the type's required fields.
Direct annotation already rejected omission, but the GENERIC call path did not:
push(toks: Token[], { kind }) — omitting required text:String — type-checked,
then reading toks[i]["text"] SEGFAULTED (null-deref, the missing String field
is a NULL pointer). Root cause: collect_and_save_subs_no_clobber (pattern.rs)
silently CLOBBERED the canonical TypeVar binding (T=Token from arg0) with the
omitting item's narrower type {kind} when they conflicted, so the arg-compat gate
then trivially passed {kind} vs {kind}.

Fix: keep the canonical binding when the candidate omits a required (non-null)
field the existing binding has (new omits_required_field guard), so the arg-compat
gate rejects the deficient item with a clear error. EXTRAS / width-subtyping are
fully preserved (only a MISSING required field trips the guard — extras never do),
as is numeric-widening-into-T inference. Omission is now a clean compile error,
not a segfault. calc lexer's 2 omission sites fixed ({kind:lparen}→add text).
570 tests + 72 corpus green. Prereq for sealed-records (packed layout needs the
no-omission guarantee).
- Merge master into fix/sealed-out-of-shape-access (pre-merge sync)
- Merge fix/sealed-out-of-shape-access: sealed out-of-shape field read → Null, not panic

Reading a field NOT in a sealed record's shape (p["wisdom"] where p: Person and
Person={name,age}) crashed codegen (panic 'sealed_field_layout: field not in
record') instead of following safe-access semantics. A sealed record holds
exactly its declared fields (extras stripped on assignment), so a statically-
absent field is provably missing → Null (spec §6.1), matching the checker's
existing warning and the boxed missing-key→Null behavior.

Fix: literal-key Index on a sealed record checks fields.contains_key and emits
Const::Null for an absent field (lower.rs); codegen field-get + sealed-array
field-get guard before sealed_field_layout, returning null_value_for(result_ty)
(data.rs). Dynamic-key p[k] on a sealed record (also crashed) now materializes
to a boxed LinObject for the dynamic lookup, RC-balanced. ASan-clean; 574 tests
+ 72 corpus green.
- Merge master into feat/intersection-types (pre-merge sync)
- Merge feat/intersection-types: record intersection & syntax

type OldPerson = Person & { "wisdom": Boolean } — & on record types produces a
record with the UNION of both operands' fields. Type-level only: resolves to a
plain Type::Object at resolution time, no codegen/IR change. & binds tighter than
| (TS convention); left-assoc; record-only (non-record operand = error); same-key
conflict with differing types = error, same type dedups. named=sealed inherited
via expand_named_body (a type T = A & B value has exactly the merged fields,
extras projected, omission rejected). Author's Person/OldPerson/sayHello example
builds + runs (width-subtyping into Person works). Formatter round-trips A & B.
SPECIFICATION §5.4 + ADR-061. 583 tests + 72 corpus + fmt green.
- Merge fix/dynamic-json-arith-null: route Json-operand arithmetic through null-safe tagged path (RAPTOR #5)
- Merge branch 'master' into worktree-agent-ad27e84dc6309cb94
- Merge master into feat/repr-pass-stage1 (pre-merge sync)
- Merge repr-pass Stages 1-2: carry.rs extraction + repr.rs observer

Stage 1: factor the union-find carry-class machinery out of escape.rs into a
shared crates/lin-ir/src/carry.rs (UnionFind, coerce_is_carry, carry-edge +
TailCall classifiers). escape.rs delegates to it; stack-alloc set bit-identical.

Stage 2: new crates/lin-ir/src/repr.rs — the representation lattice
(Unknown/Packed(Layout)/Boxed(Inner)/FlatScalar/Bottom, with the key
Boxed(WrapsPacked) keep-packed-by-pointer state the type system can't express)
+ analyze(func)->Vec<Repr> (single-pass union-find over carry classes, seed at
producers, lattice-join, fail-safe Boxed). Runs after monomorphize+lower, before
rc_elide, in both main + import pipelines. PURE OBSERVER — nothing consumes the
table yet. The Stage-2 ORACLE (debug-only): asserts the computed repr AGREES
with the existing type-driven predicates at every decide/assume site — GREEN
corpus-wide + across 588 tests, proving the analysis reproduces today's
decisions before Stage 3 swaps the decision source. Three analysis gaps found +
reconciled (all conservative-analysis bugs, no latent codegen bug). verify()
soundness gate also present (debug). No behavior change; ASan corpus clean.

Design blueprint: docs/REPR_PASS_DESIGN.md (working ref, →ADR once landed).
588 tests + 72 corpus green.
- Merge repr-pass Stage 3: codegen trusts func.repr at DECIDE/ASSUME sites

Behavior-preserving subset (byte-identical, oracle-proven): LinFunction gains a
repr: Vec<Repr> side-table; codegen reads func.repr_of(temp) / threaded arg_reprs
instead of re-deriving representation from the Type at MakeObject, MakeArray
(sealed gate), Push, FieldGet, SealedArrayFieldGet, and Index (sealed-array +
sealed-record-dynamic-key). The Stage-2 oracle is strengthened to BIDIRECTIONAL
(predicate <=> repr) at the swapped sites; verify() is now load-bearing (debug
panic on any repr/opcode mismatch). 588 + 72 green; corpus output byte-identical
vs Stage-2 base (LLVM IR diffs only in HashMap decl-emission order); ASan clean,
leak summaries identical to base.

NOT YET done (folds into Stage 4): moving coercion insertion into the pass,
swapping IndexSet/eq/release, and DELETING the three-way predicate replication —
all entangled with the BoxKeepPacked machinery and the +1/Release ownership the
oracle doesn't yet cover. Representation is now decided in one place AT the
swapped sites; full single-owner completes with Stage 4.
- Merge branch 'master' into worktree-agent-a6b490e2efd99f615
- Merge master into feat/repr-pass-stage4 (pre-merge sync)
- Merge repr-pass Stage 4 (core): BoxKeepPacked/UnboxKeepPacked + scalar keep-packed Map round-trip

Part A (complete): two IR ops — BoxKeepPacked (wrap a still-packed LinArray*/
struct* into a TaggedVal by pointer, O(1), borrows inner, no materialize) and
UnboxKeepPacked (tag-check + payload ptr load + retain). use_def (liveness) +
interference (rc_elide) arms; codegen via box_array-by-pointer / unbox_ptr; repr
seeds BoxKeepPacked->Boxed(WrapsPacked), UnboxKeepPacked->Packed.

Part B (scalar sealed arrays): emit_map_set stores a Packed(sealed array) value
via BoxKeepPacked (the 0xFE LinArray* by pointer) instead of materializing via
sealed_array_to_tagged; sealed_array_project_from dispatches on elem_tag so a
keep-packed map slot is read back zero-copy (packed), a genuinely-boxed Object[]
takes the rebuild cold path. IR-proven: scalar {String: P[]} store emits
box_array, ZERO sealed_array_to_tagged. ASan-clean store/overwrite/drop (the
keep-packed borrow vs shell-release balances; map drop frees once). 589 + 72
green.

DEFERRED (regressed corpus when forced, correctly held): Part D heap-field-array
unlock (toString/Json/out-of-shape/filter boundaries still materialize element-
wise — need keep-packed wiring) and Part C predicate-deletion / full single-owner
refactor (high-risk multi-crate, unproven byte-identical). Two PRE-EXISTING bugs
found (both on master, orthogonal): {String: P[]} map-read captured into a for-
closure segfaults (match-narrow→capture lowering); std_array_for over that shape
link-errors. Stage 5 completes Part C/D.
- Merge repr-pass Stage 5: Part C (IndexSet/Release on func.repr) + ADR-062, retire design doc

Part C (partial, byte-identical): IndexSet RHS project-vs-verbatim and emit_release
shape now dispatch on func.repr (the proven representation) rather than re-deriving
from Type — extends single-owner to two more sites; sealed_repr_differs no longer a
representation-decide predicate. The lower.rs/monomorphize coercion-insertion mirrors
remain (deleting them needs a repr.rs STEP-4 coercion-insertion pass, deferred rather
than relocated unsoundly). Byte-identical run-equivalence vs Stage-4 base; oracle +
verify hold.

Part E: the representation-pass design is now ADR-062 in DECISIONS.md (lattice,
single-owner principle, Boxed(WrapsPacked) keep-packed-by-pointer, boundary catalogue,
the verify gate that makes mismatch inexpressible) + a SPECIFICATION 5.9.1 note;
docs/REPR_PASS_DESIGN.md deleted.

Heap-field packed arrays (Stage 3b) remain BOXED — verified (gate-broaden experiment)
that they crash on the basic construct/push/read lifecycle, not just exotic boundaries;
needs real per-element keep-packed wiring (toString/Json/out-of-shape/filter + the
construct path), a characterized sound boundary, not a one-line flip. 589 + 72 green,
ASan-clean.
- Merge branch 'master' into worktree-agent-a3cbe1e74240cecc0

# Conflicts:
#	crates/lin-codegen/src/codegen/rc.rs
- Merge master into feat/heap-field-arrays (pre-merge sync)
- Merge fix: dot-call empty-literal representation drift (latent UAF) + repr::verify hardening

TWO fixes (heap-field array PACKING is NOT enabled — gate stays scalar-only):

1. Latent memory-safety bug fixed: an empty/inferred array literal as a dot-call
   receiver or argument (`[].fillRecv()`, `x.f([])`) inferred Array(Never) and
   lowered BOXED, but a concrete packed/flat-scalar T[] param's callee does packed
   stride-N push/get → producer/consumer representation drift → garbage stride +
   ptr::copy_nonoverlapping precondition violation (ASan-confirmed on master:
   [].fillArg() into Pt[] gave '1 0 0', want '1 1 4'). infer_call already routed
   array-literal ARGUMENTS through expected-type checking against a concrete param;
   infer_dot_call now does too — for both the literal ARG and the empty RECEIVER
   (checker/call.rs). The Named-alias angle was a red herring; the cause was
   Never-typed empty literals not adopting the resolved param element repr.

2. Soundness gate hardened: repr::verify extended from FieldGet/SealedArrayFieldGet/
   Index to also cover Push (array + element) and sealed IndexSet — so a
   producer/consumer representation mismatch is now a debug-build compile panic,
   not an ASan-only runtime UAF. This is the class that produced the bug above and
   the earlier push/sort UAFs. Unit test verify_catches_push_repr_drift.

Heap-field packed arrays remain BOXED (fail-safe): the map-value round-trip
({String: Neighbor[]} mutated in place AND iterated boxed) is a whole-program
representation conflict, documented in ADR-062 with the re-enable prerequisites
(named full-field runtime descriptor OR cross-module record-taint pass). 592
tests + 72 corpus green, ASan-clean.
- Merge branch 'master' into worktree-agent-a9ef2d323087451da
- Merge branch 'master' into worktree-agent-ac5f8c311f9aa8d01
- Merge master into perf/global-const-fold (pre-merge sync)
- Merge perf/global-const-fold: intern immutable val globals → GlobalOpt const-folds them

Top-level immutable `val` globals now emit with Internal LLVM linkage (was
external), letting GlobalOpt prove single-store-of-constant globals are constant
and fold them — e.g. a `val MOD = …` divisor in a hot loop becomes a magic
multiply-shift instead of a per-iteration idiv. records benchmark ~535ms→~201ms
(~2.66x) = Rust parity; identical RESULT. General: any literal-val constant/
divisor in a hot loop benefits.

Principled + sound: an `immutable: bool` on GlobalValSet/Get threads val(true)/
var(false) from lower.rs; codegen sets Internal iff immutable. A mutable `var`
global stays external (multi-store, NOT folded — genuine shared state). The
var-init-guard flag (single-store of true) is correctly excluded by the gate —
a blanket-internal would have wrongly folded it and skipped initializers. _ir_gv
slots are TU-local (cross-module vals go via a {mod}_{name}__val wrapper fn, not
the global). 598 tests + 72 corpus + ASan green; var-global mutation + cross-fn
val + non-constant val + cross-module all verified correct.
- Merge unboxed-sumtype Stage 0: checker prereqs (type-check only)

Two sound, independently-useful checker fixes (no IR/codegen/runtime change),
the no-risk warm-up for the unboxed sum-type project (design: docs/UNBOXED_SUMTYPE_DESIGN.md):

- Gap 3: infer_index now resolves a Type::Named record alias (with a cycle guard)
  before indexing, so indexing a named-record-typed value works regardless of
  origin (annotation, inferred, mutually-recursive return). Was 'Cannot index
  into type X'.
- Gap 2: match exhaustiveness now credits  arms over a discriminated union
  (distinct-StrLit discriminant) as covering each variant —
  over a union is exhaustive without a redundant else. SOUNDNESS PRESERVED:
  discriminant must be present-on-all + distinct-StrLit (else falls back to exact
  structural check); a supertype/widened arm gets no credit; a missing-variant
  match with no else STILL errors (negative tests prove it).

605 tests + 72 corpus + ASan green. Caught+reverted a fromJson regression
(multi-variant union members are open objects → keep the dynamic-field fallback).
Design doc included. Stage 1+ (the repr/codegen) follow.
- Merge unboxed-sumtype Stage 1: SumNode representation foundation (gated INERT)

The non-recursive scalar sum-type substrate, committed gated-OFF (zero behavior
change, byte-identical corpus): a heap SumNode header mirroring sealed records
([rc|size|desc|u32 tag@16|payload@24]) so lin_rc_retain/IMMORTAL_RC work
unchanged (crates/lin-runtime/src/sumnode.rs, 4 ASan detect_leaks=1 unit tests
green); a Repr::Packed(Layout::SumNode) lattice variant + strict sum_type_eligible
gate (>=2 object variants, shared distinct-StrLit discriminant, all-scalar other
fields — heap/Named/union/recursive → boxed) + oracle/verify SumNode arms; codegen
layout calculus + construct/tag-load/const-offset-field-get/materialize-to-boxed
helpers + release dispatch.

DELIBERATELY INERT: the seed is gated off (sum literals still compile boxed) —
making it live requires the call ABI (pass a sum value by SumNode pointer, read a
sum PARAM as a SumNode) which the agent's own oracle/verify correctly flagged as a
latent UAF when half-wired. Flip points marked NOTE in repr.rs. 605 tests + 72
corpus + ASan green; no new memory behavior. Foundation for Stage 2 (recursive +
the ABI wiring). Design: docs/UNBOXED_SUMTYPE_DESIGN.md.
- Merge branch 'master' into worktree-agent-ae7122bfd79be6a85
- Merge branch 'master' into worktree-examples-consolidation
- Merge branch 'master' into fix/object-get-hang
- Merge fix/object-get-hang: Json/Object value used as typed map { String: T } hangs/corrupts

Json/Object → Map coercion now materializes a real LinMap (tag-dispatched lin_to_map) instead of
reinterpreting a LinObject's bytes as a hash table; defensive find_slot probe bound. Fixes the
std/object.get infinite-loop on an absent key and the heap corruption on a present key when the
receiver is a runtime LinObject (empty {} / Json object field used where { String: T } is expected).
- Merge branch 'master' into worktree-examples-consolidation
- Merge fix/stream-failed-source-error: thread failed-source Error in-band through stream ops
- Merge branch 'master' into worktree-examples-consolidation
- Processes fold (concurrency+result) via lin-engineer agent
- Update guidance
- Merge branch 'master' into worktree-agent-a998bd9184af4ed60
- Update guidance
- Merge branch 'master' into fix/crossmodule-generic-transfer
- Merge fix/crossmodule-generic-transfer: box worker/shared transfer args on static type

Fixes a misaligned-pointer crash when a generic function imported from another module builds an
object-literal message and sends it across a worker boundary — message/request/shared/set were
passing a raw heap pointer through unboxed instead of a boxed TaggedVal. Box on the static type at
all four transfer sites. Unblocks a fully-generic (Json-free) async std/event.
- Merge fix/fmt-generic-type-app: formatter emits generic type application with angle brackets

A generic type application (Bus<T>, Map<K,V>) in a return type / binding annotation was being
formatted as Bus[T] (array syntax), which no longer parses — a meaning-changing formatter bug.
Emit <...> to round-trip.
- Merge branch 'master' into worktree-examples-consolidation
- Merge master into feat/sumtype-live (pre-merge sync)

# Conflicts:
#	crates/lin-codegen/src/codegen/data.rs
- Merge unboxed-sumtype: non-recursive scalar sum types LIVE + sound through all boundaries

A non-recursive scalar sum type (type Shape = Circle{kind:"circle",..} |
Square{kind:"square",..}, distinct-StrLit discriminant, all-scalar other fields)
now packs as an unboxed SumNode end-to-end: construct, match-is dispatch (inline
tag load+switch, NO lin_matches_schema), constant-offset field read, pass-to-fn /
return (SumNode-pointer ABI), and round-trip through Map values + array elements
(materialize-to-boxed on store, project-from-boxed on read). ~2.9x faster than the
boxed-union equivalent on construct+dispatch.

The call ABI was the crux Stage 1 deferred: sum param = SumNode pointer; caller
materializes-to-boxed at Json/union/generic/cross-module/container boundaries;
read-back projects to a fresh SumNode. repr::verify is load-bearing-green with the
seed ON across the whole suite — and was EXTENDED to cover the Coerce/Call-arg/
Return/Map sum boundaries (it now panics at compile time on a boxed value reaching
a SumNode param — the hole that let the map-value bug ship). Map-value-sum +
array-of-sum round-trips fixed (were garbage 6534 / non-exhaustive); a pre-existing
per-read sealed-projection leak in match arms also fixed. ASan-clean (no
per-iteration leak; the fixed 130B is the immortal string-interner, baseline-
identical). 613 tests + 72 corpus green; verify+oracle active.

Foundation for the recursive sum type (Stage 2 — the interp Ast). Design:
docs/UNBOXED_SUMTYPE_DESIGN.md.
- Merge branch 'master' into worktree-examples-consolidation
- Merge branch 'master' into feat/std-event
- Merge branch 'master' into feat/std-event
- Merge branch 'master' into feat/std-event
- Merge branch 'master' into feat/std-event
- Merge fix: pin generic result type-param inside record fields & union index
- Merge fix: async deep-copies captured function-value closures onto worker thread
- Merge master into feat/sumtype-recursive (pre-merge sync)
- Merge fix: mutual-recursion record-return segfault (forward-decl repr mismatch)

Two mutually-recursive functions returning a record segfaulted: when the first
of the pair is checked, a call to the not-yet-checked sibling resolves its return
type from the forward-declared signature, which still carried the UNRESOLVED
Named("R") alias placeholder. unify_types([{v:Int32}, Named("R")]) then formed a
spurious boxed Union as the if-merge/body type — so the function returned a boxed
shape while the sibling actually returned the SEALED PACKED struct; the
return-coerce read the packed-struct pointer as a box (lin_unbox_ptr) → garbage
pointer → SIGSEGV. (Self-recursion escaped it: a direct self tail-call TCOs to a
back-edge, never reading the record cross-frame.)

Fix (checker, infer_call): expand Named aliases in the resolved return type against
the fully-resolved env so a cross-function call's result carries the same
structural sealed shape the callee body produces — every box/coerce/project repr
decision then agrees. Scoped to skip a direct self tail-call (preserves Stage-4
stack-alloc of a TCO-loop accumulator record). Sound: alias expansion yields a
structurally-identical type, only removing the imprecision the repr decisions keyed
on. Repro f(5)→1 ASan-clean; sealed/boxed/String/scalar mutual-recursion + self-rec
(tail and non-tail) all correct; Stage-4 stack opt preserved. 617 tests + 72 corpus
green. Prerequisite for recursive sum types.

(Out of scope, separate pre-existing bug noted: mutual recursion returning an
ARRAY of records corrupts in the sealed-array materialization path.)
- Revert "Merge fix: pin generic result type-param inside record fields & union index"

This reverts commit 6230ecdc2837692db1c29ae6ce4eb20e94c97cb1, reversing
changes made to ae52cd5b278c2406a57a957f4355038bfc7a5ce7.
- Merge branch 'master' into feat/std-event-redo
- Merge feat/std-event: add std/event — fully generic typed event emitters (re-merge)

Restores the std/event module lost when master was hard-reset to origin/master (the earlier merge
cc8dafa was discarded by that reset). Re-merged onto current master and re-verified.

Two layers, generic over the event payload type T (no Json payloads):
  • Layer 1 (async, worker-backed): emitter<T,S> / send<T> / request<T,S> / drain<T,S> / stop
  • Layer 2 (sync, in-process): bus<T> / on<T> / once<T> / off<T> / emit<T> / listenerCount<T>,
    over exported types Listener<T>, Bus<T>, Sub.

Registered in lin-compile and lin-lsp; documented in docs/STDLIB.md and docs-site; example
examples/event-transfers. 619 workspace tests + event/example ASan + fmt-check green.
- Merge fix: erase phantom return-only generic params + pin generic result through record-field index
- Merge branch 'master' into worktree-examples-consolidation
- Merge branch 'master' into worktree-agent-a354bb527d34df5ce
- Merge branch 'master' into worktree-agent-a354bb527d34df5ce
- Merge fix: attribute monomorphized-generic coverage to the generic's source module
- Merge branch 'master' into worktree-examples-consolidation
- **stage3b**: ADR-063 + sealed-record representation verification harness
- Merge branch 'master' into docs/streams-events-tutorials
- Merge docs/streams-events-tutorials: add Streaming I/O and Events tutorials

Two new docs-site tutorials appended after Testing (no renumbering):
  • 13-streams: sources/adapters/terminals, in-band Error threading, write sinks, compression +
    archive adapters, .promise() worker-driven pipelines, single-use semantics.
  • 14-events: the synchronous bus and the async worker-backed emitter, closing with the
    stream-in/process/emit capstone linking back to streams.

nav.json updated; all snippets verified against the compiler; all internal links resolve.
- Merge branch 'master' into stage3b-design
- Merge fix: scalar Float32->Float64 return fpext + flat-array element-width coercion (UInt8[]->Int32[])
- Merge fix: adopt float literal at Float32 context type
- Merge branch 'master' into worktree-examples-consolidation
- Merge branch 'master' into fix/sort-result-leak
- Merge branch 'fix/sort-result-leak'
- Merge branch 'master' into stage3b-design
- Merge branch 'stage3b-design'
- **vscode**: Bundle extension with esbuild into dist/extension.js
- Merge feat: make 'from' a contextual keyword (usable as identifier outside imports)
- Merge branch 'master' into worktree-examples-consolidation
- Merge branch 'master' into worktree-agent-a528e485161129f02
- Merge branch 'master' into worktree-agent-ac70af97e956e35a6
- Merge branch 'master' into worktree-agent-ac70af97e956e35a6
- Merge unboxed-sumtype Stage 2: recursive sum types pack as unboxed SumNodes + tail-return nested-child pushdown fix

Stage 2 of the unboxed tagged sum-type representation (docs/UNBOXED_SUMTYPE_DESIGN.md):
recursive sum types (type Ast = Num | BinOp with recursive left/right) pack
end-to-end as unboxed heap SumNodes (KIND_SUMNODE child slots, recursive RC drop
walk, const-offset child reads, O(1) tag-switch dispatch).

Includes the tail-return correctness fix: a nested sum literal returned from a
function (direct return, if/else tail, match-arm tail) now pushes the expected
variant type into its recursive children so they construct child SumNodes instead
of boxed objects (was: garbage discriminant -> non-exhaustive match). Plus a
recursive-constructor mis-TCO fix and the if/match result-type recursive-child
preservation fix.

Verified: 849 workspace tests, 75 corpus files, fmt --check, and ASan-clean on
build-and-return at N=2000 (zero sumnode leak frames, no UAF/double-free).
- Merge branch 'master' into worktree-examples-consolidation
- Merge fix/macos-net-tcp-flake: fix flaky macOS TCP loopback net test

Two macOS BSD-socket-stack causes: accepted socket inherited the
listener's O_NONBLOCK (fixed in lin_tcp_accept) + handshake-timing
single-accept race (fixed with bounded acceptWithRetry in the test).
All CI legs green on PR #9 incl. macos-latest.
- Merge branch 'master' into worktree-examples-consolidation
- Merge examples consolidation: 16 feature-demo dirs -> 7 real-world projects

Fold the single-feature example demos into larger, idiomatic real-world projects so
examples/ showcases capabilities in context rather than 1:1 per feature:
- streams+wordcount+indexed -> report (lazy Stream CSV, {String:Int32} histogram, std/hash dedup, std/path)
- concurrency+result -> processes (generic Result outcomes, parallel/timeout/retry/threadPool scheduler, std/env)
- codec -> raspberry-controller (TLV telemetry + bit helpers, unified NAL)
- matrix+ffi -> sdl (Vec2/Mat3 physics+rotation; your-own-C FFI at sdl/clib alongside vendored libSDL3.so)
- config+dijkstra -> web-server (startup config load/validate, /route Dijkstra endpoint, std/signal shutdown)

Every consolidated project: idiomatic Lin (no Json in example signatures), >=80% per-file
coverage, tests ported. CI/test rewiring: archive-build + ASan case -> sdl/clib; dropped the
dijkstra argv run; revived the dead SKIP-passing FFI test as a hermetic build; docs swept.
- Merge branch 'master' into worktree-agent-aa64dcb8a8aee2a89
- Merge branch 'master' into worktree-agent-afa6d5e59f73b4d08
- Merge branch 'worktree-agent-afa6d5e59f73b4d08'
- Merge branch 'master' into worktree-agent-aa64dcb8a8aee2a89
- Merge branch 'worktree-agent-aa64dcb8a8aee2a89'
- Merge branch 'master' into fix/tco-loop-exit-leak-b
- Merge commit '1217079'
- Merge commit 'b0965a2'
- Merge branch 'refactor/gate-consolidation'
- Merge branch 'master' into test-consolidation
- Merge branch 'master' into test-consolidation
- Merge branch 'master' into test-consolidation
- Merge branch 'master' into worktree-agent-a97c0adb6180f7b98
- **interp**: Port AST to unboxed recursive sum type (Stage 4 payoff measurement)
- Merge branch 'master' into feat/sumtype-integration
- Merge commit '4ca115b' into feat/sumtype-reland

# Conflicts:
#	crates/lin/tests/integration.rs
- Merge unboxed sum-type Stages 3-4 + keep-packed (re-land onto master)

Re-lands the keep-packed sum-type work that was previously mis-merged onto
feat/iter-combinators instead of master. Stages 0-2 (recursive SumNode AST) were
already on master; this brings the Stage 3-4 + keep-packed delta:

- Keep-packed through boxed containers via TAG_SUMNODE: a SumNode stays packed-by-
  pointer in a record field / {String:_} map value; dynamic consumers (toString/==/
  json/worker-transfer) still materialize. Resolves the store/read repr asymmetry.
- Stage 4: interp.lin AST ported to the sum type. ~0.437s vs 0.526s Json baseline
  (vs 0.768s for the materializing intermediate). RESULT=10460000.
- Soundness fixes: untyped-object store overflow, map round-trip double-release, and
  TCO loop-exit AND back-edge param release dispatched by repr not static type (the
  latter resolving a semantic conflict with master's concurrently-landed
  emit_tco_release_final). ADR-064 + design-doc status.

Verified on current master base: 876 workspace / 652 integration / 65 corpus / fmt
all green; sumtree 12/12 stable; interp + map round-trip ASan-clean.
- Merge branch 'perf/token-alloc'
- Merge branch 'perf/foreach-closure'
- Merge branch 'master' into feat/stage3b-mechanism-i
- Merge branch 'feat/stage3b-mechanism-i'
- Merge branch 'master' into feat/stage3b-widen-string
- Merge branch 'feat/stage3b-widen-string'
- Merge branch 'fix/sealed-json-view-leak'
- Merge branch 'master' into perf/capturing-closure-inline
- Merge perf/capturing-closure-inline: inline capturing closures at literal .for/combinator sites

Relax the capture-less inliner (inlinable_lambda) to inline a LITERAL capturing lambda
at array .for / range().for / map / filter / reduce call sites, eliminating the boxed
per-element closure-call ABI (box arg + indirect call + release + free-shell).

- Captures resolve by the same outer_slot in the enclosing builder (no env struct);
  ADR-012 shared-var-cell + global-var mutation preserved automatically. Guarded by
  capture_resolvable + repr-match; bails to the boxed path otherwise.
- Latch-relative back-edge/phi wiring so a body that emits its own blocks (inner
  .filter/match/if) doesn't malform the loop CFG (the spike's hang bug).
- Array .for element-box fully released (no leak/double-free).

Measured: ~2x on a closure-ABI-bound microbench (range(0,2e8).for, 6.1s->2.9s).
RAPTOR modest (GROUP -2.4%, RANGE -4.6%) — its hot loops are body-bound (Json field
access), digest BYTE-IDENTICAL (group=26203913 range=773022892 journeys=139).

Verified: 912 workspace / 670 integration / 65 corpus green; 10 new closure tests incl.
block-emitting-body (filter+match), global-var, stream-unchanged, non-literal-unchanged;
ASan zero new leaks; RAPTOR digest verified on full GTFS feed.
- Merge branch 'fix/push-into-json-field'
- Merge branch 'fix/sealed-field-get-out-of-shape'
- Merge branch 'fix/for-sealed-string-elem-leak'
- Merge branch 'feat/widen-string-3'
- Merge branch 'feat/widen-scalar-array'
- WIP checkpoint: gather all 12 new modules + 5 enrichments (pre-consolidation, pre-wiring)
- Consolidate hash->encoding, url->http, os->process, result->object (content+deletes)
- Wire+merge compiler-wiring, relocate url/os/result tests, add bignum/decimal tests (bignum/decimal blocked on foreign scalar-union compiler bug)
- Consolidate modules + JSDoc comment pass + supporting compiler fixes
- Preserve blank line between leading comments; use it to separate module headers from first-decl docs
- Also preserve a blank between the last leading comment and its declaration
- Merge branch 'master' into integrate/stdlib-consolidation
- Merge branch 'master' into feat/widen-nested-record
- Merge branch 'feat/widen-nested-record'
- Merge branch 'fix/nested-string-record-array-iter'
- Merge branch 'master' into fix/repr-nested-sealed-array-field
- Merge branch 'fix/repr-nested-sealed-array-field'
- Merge branch 'master' into feat/sealed-kind-map
- Merge branch 'master' into feat/sealed-kind-map
- Merge branch 'feat/sealed-kind-map'
- Merge branch 'fix/narrow-sealed-gate-scalar'
- Merge branch 'master' into worktree-agent-a811f2de4ce3fd79d
- Proposals
- More paths
- Merge branch 'master' into fix/union-tailcall-uaf
- Merge fix/union-tailcall-uaf: RC-soundness for concrete values crossing the union boundary
- Merge fix/raptor-keys-null-narrow: && flow-narrowing + clean type Display; unbreak RAPTOR
- Merge master into fix/sealed-array-map-repr
- Merge fix/sealed-array-map-repr: coerce boxed combinator-result bound to packed sealed-array annotation (fixes silent data corruption)
- Objective C: fix inline range-for element-box leak (scalar->union)

range(0,N).for(n => ... dynamic-op(n) ...) bound the i32 counter to a Json/union
param via coerce_arg_to_param_repr, which emits lin_box_int32 (a fresh +1 TaggedVal
shell for values outside the small-int cache). The inline path deliberately did not
register the param bind owned, so the shell leaked ~16B/iter (303634B at N=20000).
Fix: inline_lambda_body_tracking_elem_boxes returns the scalar->union element boxes;
lower_range_for frees each shell via FreeBoxShellIfDistinct after the body (mirroring
the non-inline elem_boxes path). Shell-only + cached-box-safe. Leak 303634B->18B
(constant residual), no UAF/double-free across N=200/2k/20k + push-transfer attack;
suite 683 green; RAPTOR digest byte-identical.
- Objective C (extend): reclaim scalar->union param-bind box in ALL inline combinators

inline_lambda_body now frees each tracked scalar->union param-bind box shell
(FreeBoxShellIfDistinct vs the body result) so the inline for/map/filter/reduce/while
paths over a flat-scalar element with a Json/union lambda param no longer leak the
per-iteration box. Shell-only + cached-box-safe + result-distinct guarded.
ASan-clean (constant residual, no UAF/double-free) on for/map/filter/reduce/range-for
+ push-transfer; suite 683 green.
- Merge fix/inline-for-elembox-leak: reclaim scalar->union element-box shell in inline range-for/combinators (fixes ~16B/iter leak)
- Merge docs/perf-retrospective-dict-vs-record: dict-vs-record de-Json finding into path-n proposals
- Allow type aliases resolving to String as map keys
- **ir**: Path-6 6a combinator-chain fusion (reduce+for terminals)
- **ir**: 6b specialized dispatch — redirect concrete-typed std/array length/push to intrinsic (skip Json box)
- Merge branch 'master' into perf/integrate-inplace-fusion
- Merge perf/integrate-inplace-fusion: in-place packed combinators + chain fusion + length dispatch

Integrates two complementary, separately-verified performance wins (no userland change,
benchmark .lin unchanged — the RAPTOR full-typing regression is deliberately excluded):

- IN-PLACE packed combinators (from path1-packed-records): single-combinator reduce/map/
  for/length over a packed sealed-record array read elements by const-offset pointer with
  NO per-element materialize (~34-55x on a lone packed reduce). + Step-3 String-field read
  capability (gate stays scalar+Bool). + the Trip|Null tail-call UAF fix + a per-iteration
  index/element box-leak fix (both real RC-soundness fixes).
- COMBINATOR-CHAIN FUSION (path-6 6a): map().filter().reduce() chains fuse into ONE loop,
  no intermediate arrays, no per-stage closure call (~2.8x on the cross-lang pipeline
  workload — Lin now beats Rust/Go there; ~3.4x on tight chains).
- LENGTH dispatch (path-6 6b): concrete-receiver length()/push() redirect to the direct
  intrinsic, skipping the Json box (~1.35-1.8x on length-bound loops).

The two combinator paths coexist (disjoint by shape: single packed op -> in-place; chain
-> fused). Verified: 1026 workspace / 687 integration tests pass; lin fmt + 72/72 corpus;
ASan detect_leaks=0 AND =1-with-scaling clean on all packed/fused shapes (non-scaling
interner residual only); cross-lang correctness gate passes all 7 workloads; RAPTOR digest
byte-identical (group=26203913 range=773022892 journeys=139); cross-lang regression sweep
shows no regression (pipeline 75ms->27ms, all others flat within noise).
- Update docs
- Merge commit '6e32b624' into feat/promise-type
- Merge master (Promise<T>) into feat/shared-generic
- Merge branch 'master' into feat/array-callback-keys
- Merge feat/array-callback-keys: generic key type K for sortBy/searchBy/dedupBy
- Remove extra file
- Merge branch 'master' into perf/csv-clone-unique
- Merge branch 'master' into perf/object-eq-index
- Agent 3 findings
- **runtime**: Positional fast path for lin_object_eq + A/B harness
- Merge branch 'worktree-proposals-path10-14'
- Fix tagged-array write sinks corrupting packed sealed buffers
- RAPTOR port performance fixes (7 commits, ~23% total)
- Clean up
- Fix parenthesized function type in return position
- Fix if-merge over an opaque/Json function-call result
- Fix raspberry-controller udpBind Int32|Error type error (CI example-run blocker)
- Link-error details no longer truncated to only ld warnings
- MacOS CoreFoundation/IOKit link fix + name symbols in link-error details (PR #12)
- Merge 9C seal-propagation: fix silent corruption in nested sealed-record-array reads

A typed nested sealed-record-array read (outer[0]["items"][0]["a"]) returned
garbage on master (printed '7 0' instead of '33 44'): the producer object
literal fell to undirected inference and built a boxed array while the consumer
read it as sealed/packed. The producer now seals to match the consumer's
annotation. Gate stays scalar+Bool (checker-only, not a packing widen).

Verified: 7 0 -> 33 44, 701 integration tests, RAPTOR digest byte-identical,
gate unchanged, ASan clean. One of three salvaged wins from the Path-9 effort
(heap-field packing itself retired CLOSED-NEGATIVE).
- Merge Step 8.1: record-combinator chain fusion (~2.07x)

Widens the merged Tier-2 combinator fusion to sealed-RECORD element arrays and
array-producing terminals: recs.filter().map().reduce() over a record array
becomes a single const-offset pass, no per-stage intermediate array. Includes
two pre-existing leak/UAF fixes in the multi-stage fuser (reclaim
map-produced/source values).

Verified: 2.07x measured (2M-record chain, median-7, identical output),
705 integration + 72/72 lin tests, ASan leak-constant (no per-element scaling).
lin-ir/src/lower.rs only. Second of three salvaged Path-9 wins.
- Merge dict->Map fidelity: type RouteScanner.routeScanPosition as { String: Int32 }

Routes 783k dict reads to lin_map_get on the RAPTOR bench (fidelity/correct
typing, not a measured speedup). Re-derived on top of master's ~23% RAPTOR perf
rewrite (which had deleted the original code path). RAPTOR .lin only.

Verified: RAPTOR 9/9, digest byte-identical group=26203913 range=773022892
journeys=139. Third of three salvaged Path-9 wins.
- Merge fix/tco-leak-rebased: release TCO param-slot for heap-bearing sealed records

Genuine master-reachable scaling leak (verified by IR on master, NOT a packing-chain
artifact): a standalone heap-bearing sealed record (e.g. Trip{id:String, stops:ST[]} —
seals on master regardless of the scalar+Bool array-element gate) threaded through a
self-tail-recursive param slot had 0 sealed_release in the loop — the old slot value was
overwritten by the back-edge store with no release, leaking ~32-328 B/iter scaling.

Fix narrows tco_param_needs_release's sealed-record carve-out to PURELY-scalar records
(sealed_record_is_heap_bearing); heap-bearing sealed records now participate in TCO
param release. IR: 2 sealed_release in loop() (was 0). Regression test added; the
Trip|Null TCO UAF soundness guard still passes; full cargo test --workspace 0-fail.
- Retry relocated-binary exec on ETXTBSY in FFI rpath test (fixes parallel-suite flake)
- **codegen**: Drop dead is_heap arms from sealed_array_elem_materializer
- Revert "perf(ir): admit capturing literal lambdas at Layer-1 combinator-inline gate"

This reverts commit 4ebfd90f290cbbf7dac8984c85d82827010526e8.
- **raptor**: Fully type trips (Trip[] loader + typed Conn tuple)
- Language
- Merge branch 'master' into feat/null-coalescing

# Conflicts:
#	crates/lin/tests/integration.rs
#	docs/DECISIONS.md
- Add hashmap
- **ir**: Complete ownership_verify intrinsic table — async/worker/Shared/Stream/FromJson families + 3 UNSURE upgrades
- Merge branch 'master' into feat/tar-entries
- **raptor**: Clean-map re-port of lin-typed — kill Json (132→~3), val-not-var, ?? / optional-chaining, keep memory-efficient map repr
- Merge branch 'master' into feat/tar-entries

DECISIONS.md conflict: master claimed ADR-067 for heap-field SumNodes;
the TarEntry ADR is renumbered to ADR-068 (spec cross-ref updated).
- Merge feat/tar-entries: composable tar streaming (entries/header/body, ADR-068)
- Merge feat/tar-loader-typed: tar.gz streaming loader for the typed RAPTOR port
- **raptor**: Rounds-as-array in lin-typed — delete sortedRoundKeys/roundKeys.lin
- Stage-0 scan sentinel + baselines + probe-corrected decisions
- Merge master (branch-policy doc) into reset/main
- Merge Stage-1 D3a: anon-structural param monomorphisation (+ inner-fn rename fix)
- Merge master (generic inner-fn collision fix) into reset/main

Semantic conflict resolved in the same commit: both lanes independently added
rename_inner_fns (D3a fix-round for the anon axis; master fix for the
generic+callback axes). Kept one definition + a NOTE that every spec axis must
call it. Verified on the unified tree: 766/0 integration, 72/72, genclo +
worker-transfer + D3a matrix + leg-1 dyncheck repros all correct, sentinel exact.
- Merge master (total literal-key index) into reset/main
- Merge fix/var-capture-loop-leak: free per-iteration var-cells in inline loop bodies
- Merge master (var-capture leak fix + doc status) into reset/main
- Merge branch 'master' into feat/literal-union-keyed-record
- Merge escaping-var crash-family regression tests (both verified working)
- Merge branch 'master' into feat/indexset-returns-value
- Merge branch 'master' into reset/main

# Conflicts:
#	crates/lin-ir/src/lower.rs
- Clean up CLAUDE.md
- Merge branch 'feat/number-limit-consts'

Add integer-limit constants to std/number; fix i64::MIN lexer panic and
big-unsigned-decimal formatter flattening.
- Merge branch 'master' into reset/main
- Merge branch 'master' into feat/std-datetime
- Merge branch 'master' into feat/std-datetime

# Conflicts:
#	stdlib/number.test.lin
- Merge branch 'feat/std-datetime'
- Merge branch 'feat/datetime-offset'
- Lin engineer update
- Claude guidance
- Error messages
- Merge branch 'master' into fix/linmap-indexmap
- Manual raptor port
- Merge branch 'master' into fix/coalesce-default-infer
- Merge branch 'master' into fix/nested-match-undefined-temp
- Merge branch 'master' into fix/linmap-indexmap
- Update lin engineer
- Merge branch 'master' into reset/linobj-del-p3
- Clean up
- WIP Raptor port
- Merge branch 'master' into reset/linobj-del-p45
- Merge branch 'lane/a2-sealed' into integ/wave-a
- Merge branch 'lane/a3-boxing' into integ/wave-a
- Merge branch 'lane/a6-string' into integ/wave-a
- Merge branch 'lane/a5-rc-uaf' into integ/wave-a
- Merge branch 'lane/j2-docs'
- Merge branch 'lane/j3-docssite'
- Merge branch 'master' into lane/a4-rcelide
- Merge branch 'lane/a4-rcelide'
- Merge branch 'lane/b5-data-split' into integ/wave-b-safe
- Merge branch 'master' into lane/b7-infer-if
- Merge branch 'lane/b7-infer-if' into integ/wave-b-safe
- Merge branch 'master' into lane/b1-gate
- Merge branch 'lane/b1-gate' into integ/wave-b-safe
- Merge branch 'master' into lane/b8-rcty
- Merge branch 'master' into lane/b3-nkind
- Merge branch 'master' into lane/b4-lower
- Merge branch 'master' into lane/r0-cache
- Merge branch 'master' into lane/r0-float32
- Update lock
- Merge remote-tracking branch 'origin/master' into fix/lsp-imported-type-resolution
- Merge pull request #13 from Lin-Language/fix/lsp-imported-type-resolution

fix(lsp): resolve imported type aliases in annotations
- Merge remote-tracking branch 'origin/master'

# Conflicts:
#	crates/lin-codegen/src/codegen/data.rs
- Merge pull request #14 from Lin-Language/fix/macos-sealed-desc-alignment

fix(codegen): 8-byte-align nested pointers in sealed named descriptors (macOS ld64)
- Merge branch 'master' into lane/0xfe-inline-p1
- Merge branch 'master' into lane/0xfe-phase2
- Merge branch 'master' into lane/0xfe-phase2
- Merge branch 'master' into lane/interp-borrow-rc
- Merge branch 'master' into lane/map-value-unbox
- Merge branch 'master' into lane/seal-union-ptr
- Merge branch 'master' into lane/seal-union-ptr
- Merge branch 'master' into feat/enable-smi
- Merge branch 'master' into investigate/interp-leak
- WIP lin raptor port
- **columnar**: Struct-of-arrays record array design + runtime spike
- Merge branch 'master' into explore/columnar
- Merge branch 'master' into feat/lsp-organize-imports
- Ssh mapping
- Raptor WIP
- Update gitignore
- Update Lin Engineer
- Add extension to devcontainer
- Merge branch 'master' into feat/function-overloading
- Merge branch 'master' into worktree-agent-ae83413569c383e27
- Preserve named record type alias in Type::Object for Display/LSP
- Lin test UX — suite CodeLens, grouped Testing tree, runSuite, result surfacing
- WIP raptor port
- Merge branch 'master' into fix/nested-int-keyed-map-value-lookup
- Merge branch 'master' into fix/sealed-nkind-uint-misalignment

# Conflicts:
#	crates/lin/tests/integration.rs
- Merge commit 'ef9ec3f4' into fix/resolve-type-span-diagnostic

# Conflicts:
#	crates/lin-check/src/resolve.rs
- Clearer diagnostics for bare-key record types (leaf-accurate span + quoting hints)
- Merge master into fix/sealed-union-materialize-nkind

Conflict resolution:
- tags.rs: master added NKIND_UINT32/16/8 (12-14); branch added NKIND_INT16/8 (15-16).
  Removed duplicate constant block; kept both in nkind_size_align.
- types.rs: master mapped Int8/Int16 → NKIND_INT32; branch gave them NKIND_INT8/16.
  Kept branch's finer-grained mapping (correct slot-width encoding).
- sealed.rs: auto-merge duplicated NKIND_UINT32/16/8 arms in 3 match blocks.
  Removed master's duplicates; kept branch's (use u64 not i64 cast for unsigned).
- map.rs: both sides added null guard to lin_map_set_int. Kept branch comment.
- index.rs: single conflict in compile_ir_index_set Union branch; kept master's
  cleaner implementation (functionally equivalent to branch's).
- Merge branch 'master' into feat/mismatch-drilldown
- Merge branch 'master' into worktree-agent-ab560c69ca6b615aa

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-a008cd062ba733611

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-ac764fb135f94b98c

# Conflicts:
#	crates/lin-check/src/checker/function.rs
#	crates/lin/tests/integration.rs
#	docs/DECISIONS.md
- Merge remote-tracking branch 'origin/master'

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-a653d7d7c381406fb
- Merge branch 'master' into worktree-agent-a653d7d7c381406fb
- WIP raptor port
- Update docs
- Merge branch 'worktree-agent-a9b0880287112c0c3'

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into fix/seed-cyclic-type-aliases

# Conflicts:
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-a788c047a04ba08ef

# Conflicts:
#	crates/lin-check/src/checker/mod.rs
#	crates/lin/tests/integration.rs
#	docs/DECISIONS.md
- Merge branch 'master' into worktree-agent-a983bee0e6fe846d3

# Conflicts:
#	docs/DECISIONS.md
- WIP raptor port
- Merge branch 'master' into worktree-agent-a268cf1f639ab6a92
- Merge branch 'master' into worktree-agent-acec1e10e684d729a
- Merge branch 'master' into worktree-agent-a1323dad4c9fba251
- Merge branch 'master' into worktree-agent-a2c4f19e45d445805
- Merge branch 'master' into worktree-agent-af1a6b7ef88a98c32

# Conflicts:
#	crates/lin/tests/integration.rs
- Merge branch 'master' into feat/utility-types

# Conflicts:
#	crates/lin/tests/integration.rs
- WIP raptor port
- Fixes for raptor port
- Merge branch 'master' into bedrock-arrspread
- Merge branch 'master' into bedrock-arrspread
- Finish raptor port
- Merge branch 'master' into bedrock-pack
- Revert "fix(codegen): keep sealed-record arrays packed when boxed into a union slot"

This reverts commit eab8e3ac0413f9d5f173d30e4782d9c4c05f117c.
- Add freeze note
- Merge branch 'master' into bedrock-keeppack2

### Performance

- Performance improvements
- Performance improvements in stdlib
- Use lin_object_values/lin_object_entries intrinsics for single-pass values/entries
- **lin-runtime**: Cache small-int and bool boxes (CPython-style interning)
- **stdlib**: Range() uses the lin_range builtin instead of 4 closures
- **lin-runtime**: Widen small-int box cache to [-128, 1024)
- **codegen**: Mark user/closure functions nounwind
- **codegen**: Re-apply nounwind attrs after codegen-split refactor
- **runtime**: Intern string literals (immortal cache)
- **object**: Faster object-literal construction (~10% on object_access)
- **stdlib**: Append/prepend/groupBy runtime intrinsics (TODO #597, #600)
- **array**: Route map/filter/reduce through flat lin_* intrinsics with representation-safe reads (ADR-068)
- **array**: Zero-per-element-box map/filter/reduce pipeline (~10x at -O2, ADR-069)
- **lin-ir**: Borrow the container base of an Index/IndexSet (RC elision)
- **codegen**: Inline flat scalar-array reads instead of calling the runtime accessor
- **codegen**: Fold flat-array bounds check to a single unsigned compare
- **runtime**: Single-allocation LinObject — header + entries in one block
- **codegen**: Drop dead string_release on immortal interned object keys
- **codegen**: Inline scalar-only object literal construction
- **codegen**: Inline object construction for RC-typed fields (Phase 2)
- **stdlib**: Add O(1) byteAt string primitive (fixes O(n^2) Lin-side scanning)
- **codegen**: Inline lin_string_byte_at (O(1) byte accessor) — +20%
- **codegen**: Inline flat-scalar array PUSH and SET hot paths
- **ir**: Cheap StrLit discriminator for closed-concrete-union `is V`
- **runtime**: Lazy O(1) hash side-index for large Json objects (RAPTOR #4b)
- **raptor**: Type createRaptor index maps as { String: T } (O(1)); fix nested-map codegen
- **raptor**: Type ScanResults bestArrivals/kArrivals as { String: Int64 } maps
- **sort**: Inline unboxed scalar merge sort for capture-less literal comparators
- **link**: --gc-sections / -dead_strip to drop unreferenced runtime in link
- **test**: Compile test files in parallel
- **codegen**: Intern immutable top-level val globals so GlobalOpt folds them
- **ir,codegen**: Fuse arr[i].field over boxed Object[] of sealed record (BoxedArrayFieldGet)
- **ir**: Fuse range(a,b).for(f) into a counted loop
- **lin-ir**: Inline capturing closures at literal .for/combinator call sites
- **std/csv**: Fix O(n²) parse — use short-circuiting while, not range().for
- Perf
- **ir**: Path-1 Step 1+2 — in-place packed-array for/length, no materialize
- **ir**: Path-1 Step 1+2 — in-place packed map/reduce field reads
- **ir**: Path-1 Step 3 — in-place String field read capability; gate stays scalar+Bool (oracle blocker)
- **ir**: Eta-expand bare-fn combinator callbacks for inline+direct dispatch
- **ir**: Inline-dispatch .for() by routing it through the lin_for intrinsic
- **ir**: Inline-dispatch .while() via an inline lower_while loop + lin_while routing
- Perf update
- **runtime**: O(1) hash-index inner lookup in lin_object_eq for large objects
- Drop per-row CSV header clone and per-element double-probe in unique
- **runtime**: Positional fast path in lin_object_eq (same-shape compare, zero-alloc)
- **raptor-bench**: Cache time parsing in the GTFS loader (mirrors node TimeParser)
- **raptor-bench**: Compute round keys once per round, not per access
- **raptor-bench**: Hoist runsOn key strings to once per scan/pre-filter
- **raptor-bench**: Hoist scanBack loop invariants out of the backward scan
- **raptor-bench**: Replace sameTrip with the boolean it computes
- **raptor-bench**: Resolve loader column indices once per file
- **raptor-bench**: Early-exit overtakes check, hoist interchange, defer previousArrival, single transfers read
- **ir**: Step 8.1 — widen combinator fusion to sealed-record sources + array-producing terminals
- **raptor**: Type RouteScanner.routeScanPosition as { String: Int32 } map
- **stdlib**: Object.pick bind-once (review #6)
- **stdlib**: BuildQuery uses string.join not O(n^2) joinAmp (review #3)
- **stdlib**: Array.chunk inner copy via slice not boxed push loop (review #7)
- **stdlib**: Csv scanners tail-recursive, not range().while (review #1)
- **stdlib**: Csv trimAsciiWs tail-recursive, not range().while (review #2)
- **runtime**: Decode union probe via save-len/truncate, not path.clone (#5)
- **runtime**: Decode array-index path via write!, not format! (#3)
- **runtime**: Float-to-string via stack buffer / direct write!, not format! (#9)
- **runtime**: Display stringifier appends into one buffer, not Vec+format!+join (#6)
- **runtime**: Decode object fields via byte-key lin_object_get_bytes, no temp LinString (#4)
- **runtime**: Flat-scalar get_tagged routes ints through the box cache (#2)
- **ir**: Admit capturing literal lambdas at Layer-1 combinator-inline gate
- **runtime**: ToString small-int cache — immortal LinString per [-128,1024) int
- **runtime**: Utf8Bytes via one-memcpy lin_string_utf8_bytes intrinsic (#11)
- **runtime**: FromCodePoints via one-pass lin_string_from_code_points intrinsic (#4)
- **runtime**: Single-pass tryParseInt32/Float64 via lin_try_parse_* intrinsics (#10)
- **ir**: Fuse flatMap as a push-model loop-nest stage
- **ir**: Recognize monomorphized flatMap spec + fuse empty-inner
- **ir**: Read narrowed sum-variant scalar/child field directly off SumNode
- **ir**: Re-land capturing-lambda combinator inline + fix loop-body alloca stack overflow
- **ir**: Wave C commit 1 — callback-identity spec axis for find/some/every
- **ir**: Wave C commit 2 — substitute callback param with L (devirt)
- **ir**: Wave C commit 3 — regression test + WAVEC.md
- **ir**: Complete Wave D flatMap fusion (string-inner, lone, barrier-split)
- **repr**: Admit heap-field variants to discriminated SumNodes — const-offset reads, no materialization
- **raptor-typed**: De-materialize hot scan + PREP per-stop reads
- **runtime**: Hash-filter probe in lin_map_get — skip key_eq on hash mismatch
- **runtime**: Fix map profiling init order + use AtomicU8 state machine
- **ir**: Elim intermediate LinObject when object literal flows into sealed record param
- **codegen**: Sealed_project_from — per-tag force+direct-getter, no whole-source copy
- **runtime**: Small-string freelist + int-key serialization fix
- **rc-elide**: Replace bounded BFS with post-dominator chain walk
- **runtime**: Widen small-int cache from [-128,1024) to [-128,65536)
- **map**: INITIAL_CAP 8→4 — small maps alloc 4 slots not 8 (-1.4GB RAPTOR RSS, digest exact, byte-identical IR)
- **map**: Value-unbox LinMap slots — homogeneous maps store 8B payload not 16B TaggedVal
- **rc-elide**: Convention-aware elision across Borrow calls and intrinsics
- **map**: Birth materialized record-maps MIXED to eliminate value-unbox churn
- **smi**: Enable SMI integer boxing under the smi feature flag
- **check**: Seal single-pointer union fields in records (interp Cursor fix)
- **stdlib**: Replace some/every/find lin_while impl with lin_some/lin_every/lin_find intrinsics — PREP 41s→10s

### Refactor

- **stdlib**: Use dot-application style throughout
- **lower**: Centralize container-insert RC rule into transfer_into_container
- **codegen**: Split codegen.rs into a module tree (ARCH cleanup Phase 2)
- **codegen**: Extract rt_* runtime decls into RuntimeFns struct (ARCH cleanup Phase 4)
- **codegen**: Builder façade trait removes build_*().unwrap() noise (ARCH cleanup Phase 3)
- **check,parse**: Split checker.rs & parser.rs into module trees (ARCH cleanup Phase 5)
- **stdlib**: Use index-assign/.for sugar instead of Rust FFI
- **codegen**: Remove four unused closure-wrapper forwarders
- **template**: Rename .lint template files to .jinja
- **stdlib**: Move iterable combinators to std/iter (mechanical relocation)
- **fmt**: Single comment-preserving format_source shared by CLI + LSP; drop duplicate VSCode formatter
- **ir**: Extract shared carry-class machinery into carry.rs (Stage 1)
- **stdlib/event**: Use std/object.get now that the Json/Object→Map coercion is fixed
- **stdlib/event**: Make std/event fully generic — no Json payloads
- **repr**: Consolidate the Stage-3b packability gate to one source of truth
- **std/crypto**: Compose hex/UTF-8 plumbing in Lin over std/encoding, drop 9 redundant intrinsics
- **repr**: Delete never-emitted Path-9 keep-packed IR machinery
- **lower**: Unify index/packed loop emitters behind emit_combinator_loop
- **lower**: Re-express lower_while via emit_combinator_loop
- **lin-ir**: Consume ownership fact for Index result lifetime
- **lin-ir**: Consume ownership fact for owning-read/store strategy
- **lin-ir**: Consume ownership fact for borrowed-container-base gate
- **ir**: Migrate record_escape_alias gate into ownership_verify::escape_alias_convention
- **ir**: Migrate transfer_into_container fresh-vs-retain decision into ownership_verify::container_insert_convention
- **ir**: Migrate box-shell reclaim (made_fresh_box) decision into ownership_verify::box_shell_reclaim
- **ir**: Migrate bound-box inner-+1-move (made_fresh_box) decision into ownership_verify::bound_box_moves_inner
- **ir**: Route inline owning-read trichotomies (FieldGet + boxed-array field) through own_for_read → ownership_verify::owning_strategy
- **raptor-typed**: Replace intOr helper with ?? coalesce
- **codegen,check**: Delete compiler-confirmed dead code (Group 1)
- **ir,check,codegen**: Delete 3 more dead-code items (Group 2)
- **check**: Rename is_json/is_json_dynamic → is_any_val post ADR-069 rename
- **check**: Rename json_type() → any_val_type() post ADR-069 rename
- **codegen**: Split data.rs into data/{array,object,index,coerce}.rs
- **check**: Extract join_branch_types from infer_if merge logic
- **check**: Hoist packed-vs-boxed gate predicates to Type canonical methods
- **ir**: Hoist is_concrete_rc_ty to ir.rs, delete rc_elide/ownership_verify copies
- **ir**: Split lower.rs god-file into lower/ module tree
- **tags**: Single-source nkind→size table in lin-common, eliminate dual derivations
- **codegen**: 5 strictly behaviour-preserving cleanup tasks
- **check**: Replace bespoke Type::TarEntry with generic Type::Opaque(name)
- **stdlib/csv**: Type leaf scanner returns as records instead of AnyVal
- **stdlib**: Fold rangeStep into a 3-arg range overload
- **stdlib/csv**: Drop stale AnyVal/recursion workarounds; the 3 bugs are fixed
- **stdlib/csv**: Type the stringify path; csv.lin is now AnyVal-free
- **check**: Extract shared capture-recording loop (dedup dot-call fix)
- **codegen**: Remove dead sealed_array_project_from (orphaned by bug#2 fix)
- **check**: Desugar dot-calls to prefix calls, collapse mirrored resolution (Cluster 3)
