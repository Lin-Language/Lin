# Compiler cleanup — soundness, RC/safety, stdlib cliffs, and AnyVal de-proliferation

> Working brief for the cleanup campaign that followed the 2026-06-23 full-codebase audit
> (5 parallel read-only audit agents). It captures **what we missed**, divides ownership with the
> concurrent **record-representation unification** agent, and sequences the remaining work for
> maximum parallelism. Findings are cited to `file:line` as audited.

---

## 0. Coordination — who owns what (READ FIRST)

A separate agent owns the **record-representation unification** ("a record's boxed form is *always*
`TAG_RECORD`; `TAG_MAP` is reserved for genuine `{String:T}`/AnyVal; it is structurally impossible to
store a record as `TAG_MAP`"). Its five pillars + sequence:

1. `LIN_VERIFY_REPR` report-only verifier (mirrors `LIN_VERIFY_RC`).
2. **Pillar 1** — unify boxing → always `TAG_RECORD`; **delete the `boxing.rs:104` sealed→`TAG_MAP` arm**.
3. **Pillar 2** — record arrays keep their sealed array through tuples + generics; **retire
   `lin_sealed_*_to_tagged` element-materialization** (this is also the perf fix for audit-finding B).
4. **Pillar 3** — **delete `materialize_sealed_to_map`**; convert `toString`/`keys`/`eq`/JSON/transfer
   to descriptor-driven `record_field_iter` over the packed struct.
5. **Pillar 4/5** — flip `LIN_VERIFY_REPR` to a hard CI gate.

**That agent therefore OWNS these files/areas — DO NOT touch them in this campaign:**
`codegen/boxing.rs`, the sealed-array/record-materialize arms of `lower/coerce.rs`,
`codegen/data/array.rs` `sealed_*_to_tagged`, and `runtime/sealed.rs` `materialize_sealed_to_map` +
the `toString`/`keys`/`eq`/JSON/transfer descriptor-iterator conversion. **Audit findings B (the
materialization class) and the `materialize_*` deletions are THEIRS — folded into their principle, not
patched here.**

**This document owns everything else** (soundness, RC/safety, stdlib, AnyVal). Overlap points are
flagged per workstream with an explicit hand-off.

---

## 1. The principle for THIS campaign

The audit's meta-lesson: narrow hand-written repros hid whole classes of bug because the *value* was
always correct after a round-trip — only soundness edges, RC balance, and representation were wrong.
So every item here ships with (a) a **differential stdout probe** (master vs change, real-program
shape) and (b) the **`lin build benchmarks/compare/raptor/.../bench.lin` build-only gate** in addition
to `cargo test` + `LIN_VERIFY_RC` + ASan. "Tests pass" is necessary, not sufficient; RAPTOR is the oracle.

---

## 2. Workstreams (file-disjoint, parallelizable)

### WS-A — Soundness bugs (highest priority: silent wrong results)

**A1 🔴 `is`/`match` on a union with record members accepts ANY record.**
`square is (Circle | Int32)` → `true`; `match` picks the wrong arm; the narrowed binding then reads
fields off a value that lacks them (wrong-offset reads). Single-record `is` was made structural
(`TypeCheckDeep`/`MatchesSchema`); the **union-of-records case was left on a bare
`tag == TAG_RECORD || TAG_MAP` check.**
- Root: `checker/pattern.rs:216-234` routes a `Type::Union` target to `TypeCheck(union)` (not
  `TypeCheckDeep`); `lower/match_.rs:828-843` + `lower/expr.rs:1831-1837` emit `IsType{ty:union}`;
  `codegen/match.rs:57-72` (`compile_ir_is_type_single`) reduces each object/record member to a tag-class compare.
- Fix: route each record/object member of a union `is`/match target through the structural
  `MatchesSchema` path (as single-record targets already do), or emit a per-member `TypeCheckDeep`.
- Gate: differential probe (union of distinct-shape records; `is` + `match` + the narrowed-field-read
  follow-on must all be correct) + the standing `is`-on-`T|Null` probe. **Owns: `checker/pattern.rs`,
  `lower/match_.rs` + `lower/expr.rs` `Is` arms, `codegen/match.rs`.** Coordinate with the repr agent
  only if they touch `codegen/match.rs` for descriptor work (they shouldn't — Pillar 3 is runtime-side).
- Note (low sev, same class): `type_tag` maps `Int8/16/32/IntLit`→`TAG_INT32`, so `is Int8` inside a
  union matches an `Int32` value — consistent with shared physical tag; decide intended semantics.

### WS-B — RC / memory-safety bugs + landmines

**B1 🟠 `lin_tagged_release` missing `TAG_FUNCTION` arm — real leak (one line).**
`tagged.rs:652-672` has no `TAG_FUNCTION` case (falls to `_ => {}`), but `retain_tagged_payload`
(`:707-712`) and `release_tagged_payload_pub` (`~:750`) both handle it → a boxed `Function` (closure in
a union/AnyVal) cloned then released leaks the closure + captured env. Add
`TAG_FUNCTION => lin_closure_release(payload)`. **Owns: `runtime/tagged.rs`.**

**B2 🟠 `emit_release` (type-only) has no SumNode/packed arm — UAF.**
`codegen/rc.rs:307-360` (`:348`) falls `TypeVar|Union → lin_tagged_release` (reads offset-0 as a tag);
a `Packed(SumNode)`/`PackedStruct` value with a `TypeVar`/`Union` static type reaches it via the
**FieldSet old-value release** (`index.rs:1012`, `mod.rs:2479/2543/2567`) → wrong-tag release →
corruption. Make these sites repr-aware (route to `emit_release_repr`). **Owns: `codegen/rc.rs` +
the FieldSet release call-sites.** (The Retain path `mod.rs:1336` + Release IR op `mod.rs:1365` are
already repr-gated — leave them.)

**B3 🟠 `frozen` 0xFD→0xFE repack frees an `rc==1`-but-aliased shell — UAF, interacts with OUR merged borrows.**
`frozen.rs:108-114,229` decides "exclusively owned → free shell" on `refcount == 1`. The
**BOUND-ELEM / LAZY-MAT** (lazymat shelved, but BOUND-ELEM merged) and RC-ELIDE borrow optimizations
can leave a borrowed element at rc==1 while a borrow is live → repack frees a shell the borrow holds.
Audit reachability against the merged borrow paths; tighten the exclusivity check (true sole-owner
proof, not rc==1) or exclude borrowed-reachable graphs. Also `:198-199,219,228` reads stride/size from
**element 0 only** → heterogeneous-size 0xFD array (union of differently-sized records) over-reads +
deallocs with wrong Layout. **Owns: `runtime/frozen.rs`.** HIGH-priority manual look (live freeze code).

**B4 (landmines — correct today, fix before the relevant feature wires):**
- `columnar.rs:341-349` `lin_columnar_array_release` **missing `IMMORTAL_RC` guard** (UAF once columnar/freeze wires).
- `sumnode.rs:49` `KIND_SUMNODE = 4` numerically collides with `sealed.rs:85` `KIND_MAP = 4`; `sumnode::release_field` has no MAP arm → wild free the moment a sum variant carries a `*LinMap` field. Reuse `sealed::KIND_*` constants.
- `string.rs:351-360,367,1170` `lin_string_concat`/`build_n`/`join` u32 length **overflow → heap overflow** (>4 GiB); use `checked_add` + fault.
- `codegen/data/index.rs:1046` `compile_ir_sealed_array_field_get` casts `arr.into_pointer_value()`
  **with no guard** (the session's panic site; fixed upstream by repr seeding, but the site is unguarded
  unlike its two siblings). Add `if !arr.is_pointer_value() { return null }` to match
  `compile_ir_index`/`compile_ir_field_get`. **Coordinate:** the repr agent's Pillar 2 touches sealed
  arrays — apply this guard whichever lands second.
- `shared.rs:154,180` RwLock poison → panic across the `extern "C"` release boundary (UB/abort) if a
  `with_lock` closure faulted under the write lock; handle the poisoned case.
- `sealed.rs:820-827` `build_heap_desc_from_named_desc` drops `NKIND_SUMNODE` fields (leak; reachability-gated).
  **Coordinate:** `sealed.rs` is the repr agent's Pillar-3 file — hand this to them or sequence after.

### WS-C — stdlib perf cliffs

**C1 `array.unique` allocates a `format!` string + string-hashes per element** (`stdlib/array.lin:182-191`
via `lin_value_key`→`tagged_to_key_string`, `string.rs:1038`). Scalar dedup (GTFS ID columns) should key
on `lin_map_get_int` — zero alloc, int hash. Add a scalar fast path. **Owns: `stdlib/array.lin` (+ maybe
a runtime int-key helper).**
**C2 `iter.sliding` retains the whole source** (`stdlib/iter.lin:305-319`): `buf` grows to O(n) for an
O(w) window — trim the leading element once `len(buf) > w`. **Owns: `stdlib/iter.lin`.**
**C3 `std/string.at` byte-vs-codepoint inconsistency** (`stdlib/string.lin:189` uses byte length for
negative-index wrap; `lin_string_char_at` is byte-indexed) — module advertises codepoint-aware. Decide +
align (correctness-adjacent). **Owns: `stdlib/string.lin` + `runtime/string.rs` char ops.**

### WS-D — AnyVal de-proliferation (strategic)

**The problem.** `AnyVal` is `Type::TypeVar(u32::MAX)` — a magic sentinel overloading the TypeVar
mechanism (every TypeVar handler special-cases `MAX`). It should be a **deliberate escape hatch for
genuinely-dynamic value data only**, but it's **281 refs across the compiler** (149 lin-check, 87 lin-ir,
32 lin-codegen) and crops up as three distinct things — two of which are smells:

1. **Genuine escape hatch** (untyped wire data, JSON, recursive ASTs) — *keep*.
2. **Polymorphic placeholder in intrinsic signatures** (`intrinsics.rs`, 42 refs: `for`/`map`/`keys`/
   `parallel`/`arrayAllocate` callbacks + returns typed `AnyVal`; `Stream<AnyVal>`) — **should be
   generics `<T>`**. Using AnyVal here erases the element type at the intrinsic boundary, forcing boxing
   and defeating monomorphization downstream — a likely contributor to the materialization class.
3. **Inference fallback** (`_ => Type::TypeVar(u32::MAX)` at `checker/function.rs:318`, `call.rs:337`;
   rest-pattern binds at `stmt.rs:191`; etc.) — **silently widens to the escape hatch** when inference
   *should* either resolve a concrete type or error. These are the dangerous ones: they make AnyVal the
   *default*, not the exception.

**Plan (D is itself staged; start read-only):**
- **D0 — Census + categorize (read-only).** Tag all 281 sites as (1) keep / (2) generic-able / (3)
  inference-fallback-to-remove. Produces the elimination work-list + a measure of how much is removable.
- **D1 — Intrinsic signatures → generics.** Convert the (2) intrinsics from `AnyVal` to `<T>` where the
  type is recoverable (the callback element type, the array element type). Each is a contained checker
  change; verify monomorphization picks up the concrete type (fewer boxings downstream — measure on
  RAPTOR/interp).
- **D2 — Kill inference fallbacks.** Replace `_ => TypeVar(u32::MAX)` defaults with proper inference or a
  diagnostic; a value should reach AnyVal only by *explicit* annotation, never by the checker giving up.
- **D3 — (stretch) make AnyVal a real type, not `TypeVar(u32::MAX)`.** The sentinel-TypeVar overload is
  the root fragility (every TypeVar site must remember `MAX` is special). A dedicated `Type::AnyVal`
  variant removes that footgun. Large; scope after D1/D2 show the residual.
- **Coordinate:** D overlaps the repr agent at the *type-erasure boundary* (their TAG_RECORD/AnyVal split
  is the runtime mirror of D's static split). Share the "what is legitimately AnyVal" definition; D works
  the static-type side, they work the runtime-repr side. **Owns: `checker/intrinsics.rs`, `checker/`
  inference defaults, `types.rs` (D3).**

---

## 3. Parallel sequence

All four workstreams are file-disjoint from each other and (per §0) from the repr agent, so they run
**concurrently**:

| wave | lanes (parallel) | depends on |
|------|------------------|-----------|
| **now** | **A1** (soundness), **B1**+**B2**+**B3** (RC/safety — 3 disjoint files: tagged.rs / rc.rs / frozen.rs), **C1/C2** (stdlib), **D0** (AnyVal census, read-only) | — |
| **next** | **B4** landmines (each tiny, disjoint), **D1** (intrinsics→generics) | B4 `index.rs:1046` + `sealed.rs` items coordinate with repr agent's landing order |
| **then** | **D2** (kill inference fallbacks), **C3** | D1 |
| **stretch** | **D3** (`Type::AnyVal` variant) | D1+D2 measured |

Hand-offs to the repr agent: B4's `sealed.rs` SumNode-descriptor leak and the `index.rs:1046` guard
(whoever lands second applies it); D's AnyVal definition shared with their TAG_RECORD/AnyVal split.

---

## 4. Full audit findings (durable record)

The 5 read-only audit agents (2026-06-23), verbatim severity:

- **lin-ir lowering (materialization class):** root cause `is_union_ty` (`coerce.rs:242`) conflates
  *representation* with *type-kind* (lumps `TypeVar`/`AnyVal`/`Named`/`Union`/`Stream`/`Promise`/`Opaque`).
  → record→generic/union/AnyVal arg boxes (`coerce.rs:558-616`); `{...rec}` spread/embed materializes
  (`expr.rs:1103-1159`); sealed array→generic param O(n) boxes (`coerce.rs:455-483`); heap-field-element
  arrays stored boxed (most real records have a String field → never packed); union return boxes
  (`coerce.rs:628-638`). **→ owned by the repr agent (Pillars 1-3).**
- **soundness:** A1 (union `is`/match record-member bare-tag check) — the one live correctness bug; the 3
  prior `is`/null bugs are confirmed fixed.
- **runtime RC:** B1 (tagged_release TAG_FUNCTION leak, verified), B3 (frozen repack rc==1-aliased +
  heterogeneous-stride), B4 (columnar immortal guard, KIND collision, string overflow, shared poison,
  sealed SumNode-desc leak). The core immortal-guard discipline is otherwise sound.
- **codegen repr:** B2 (`emit_release` no-SumNode arm), `index.rs:1046` unguarded cast,
  `ir_as_raw_ptr` trust (the open sealed↔open-record box bug). Several agent flags were false positives
  (guarded by callers) — excluded.
- **stdlib:** C1 (`unique` per-element format!), C2 (`sliding` O(n) mem), C3 (`at` byte/codepoint). Most
  historic cliffs already fixed.
