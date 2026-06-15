# Memory Management in Lin

Lin uses **deterministic reference counting (RC)** for all heap-allocated values. There are no GC pauses, no background threads, and no programmer annotations required for the common case. This document explains the design, the runtime layout, the compiler strategy, and the roadmap for future improvements.

---

## Heap-allocated types

Five types live on the heap and carry a `u32` refcount as their first field:

| Type | Runtime struct | Layout |
|---|---|---|
| `String` | `LinString` | `refcount:u32 \| len:u32 \| data:[u8]` |
| `T[]` (tagged) | `LinArray` (elem_tag=0xFF) | `refcount:u32 \| elem_tag:u8 \| _pad \| len:u64 \| cap:u64 \| data:*LinArrayElem` |
| `T[]` (flat scalar) | `LinArray` (elem_tag≠0xFF) | same header; `data` points to raw `T` elements |
| `{…}` object | `LinObject` | `refcount:u32 \| len:u32 \| cap:u32 \| _pad \| entries:*LinObjectEntry` |
| `(…) => …` closure | `LinClosure` | `refcount:u32 \| _pad:u32 \| fn_ptr:ptr \| env_ptr:ptr \| env_size:u64 \| default_desc:ptr \| capture_desc:ptr` (48 bytes; ADR-034) |

Scalars (`Int32`, `Int64`, `Float32`, `Float64`, `Bool`, `Null`) are stored unboxed as LLVM primitives and carry no refcount.

### Tagged union (`TaggedVal`)

Union-typed (`AnyVal`, unresolved `TypeVar`) values are heap-boxed as `TaggedVal { tag:u8, _pad:[u8;7], payload:u64 }`. The payload is either an inline scalar or a pointer to one of the types above. `lin_tagged_release` decrements the inner pointer's refcount then frees the 16-byte box.

---

## Reference counting mechanics

### Retain / Release functions

| Function | What it does |
|---|---|
| `lin_rc_retain(ptr)` | Increments `*(ptr as *u32)` — generic, works on any RC type |
| `lin_string_release(s)` | Decrements refcount; frees the single allocation when zero |
| `lin_array_release(arr)` | Decrements refcount; when zero, **recursively releases** all heap-typed elements (TAG_STR, TAG_ARRAY, TAG_OBJECT, TAG_FUNCTION) then frees header + data buffer |
| `lin_object_release(obj)` | Decrements refcount; when zero, **recursively releases** all keys (always `LinString*`) and heap-typed values, then frees entries + header |
| `lin_closure_release(ptr)` | Decrements refcount; when zero, **recursively releases owning captures** via the closure's capture descriptor (offset 40), then frees the env allocation (size at offset 24) and the 48-byte closure struct (ADR-034) |
| `lin_tagged_release(p)` | Releases the inner heap value, then frees the `TaggedVal` box |

The recursive release in `lin_array_release` and `lin_object_release` means **nested structures are freed correctly without compiler assistance**. Flat scalar arrays (int/float elements) have no pointer payloads and skip recursion.

### Closure struct layout (32 bytes)

```
Offset  Field       Type    Notes
0       refcount    u32     Incremented on retain, freed when reaches 0
4       _pad        u32     Alignment padding
8       fn_ptr      ptr     Pointer to LLVM function (env_ptr, args...) -> ret
16      env_ptr     ptr     Pointer to heap env struct; null for non-capturing closures
24      env_size    u64     Byte-size of env allocation; 0 for non-capturing closures
```

All closures — capturing and non-capturing — use this uniform 32-byte layout. Non-capturing closures have `env_ptr = null` and `env_size = 0`. The uniform calling convention `fn_ptr(env_ptr, args...)` is unchanged.

---

## Compiler RC strategy

RC is inserted, optimised, and lowered entirely through the **LinIR pipeline** — the sole lowering path. (The earlier TypedAST-direct backend, which inserted release calls by hand at consumption points, has been removed; do not look for it.) The flow is:

```
TypedModule
  → lower.rs   : scope-frame ownership inserts pessimistic Retain/Release/CloneBox/FreeBoxShell
  → liveness.rs: backward-dataflow per-instruction live sets
  → rc_elide.rs: Perceus-style elision of provably-redundant balanced pairs
  → codegen/rc.rs: each Retain/Release lowered to a repr- or type-dispatched runtime call
```

### Lowering (`lin-ir/src/lower.rs`)

Lowering inserts RC **pessimistically** — the `rc_elide` pass removes provably-redundant pairs afterward (`lower.rs` line 8 comment). Ownership is tracked by a stack of **scope frames** (`scope_owned`): every freshly-owned heap/union temp is `register_owned`-ed at its origin and released when its scope pops (`pop_scope_releasing_keep`). Container inserts, call-argument boxing, var/global stores, index/field projections, and tail calls each adjust ownership at their site. The model: each owned reference has exactly one releasing owner; transfers move the obligation; borrows net to zero.

The tail-call path is special: scope-exit releases emitted after a `TailCall` would land in a dead `tco_post` block and never run, so owned temps are drained on the *live* block before the back-edge (`release_owned_for_tail_call`).

### Codegen (`codegen/rc.rs`)

`Retain`/`Release` are lowered to runtime calls dispatched on the value's **physical representation** (`emit_release_repr`), not just its static type — so a sealed packed record, a packed sealed array, a `SumNode`, a plain object, and a boxed union each get the correct releaser (`lin_object_release`, the sealed releasers, `lin_sumnode_release`, `lin_tagged_release`, …). Non-packed reprs defer to the type-based `emit_release`.

---

## Perceus elision (RC elision pass)

The elision pass in `lin-ir/src/rc_elide.rs` implements a conservative approximation of the Perceus algorithm (Reinking et al., PLDI 2021):

1. Run liveness analysis over the function.
2. For each `Retain { val }`, search forward for a matching `Release { val }`.
3. If the path between them contains **no** call, heap allocation, or aliasing release — the value was never shared, so both instructions are redundant and are removed.

**Current limitations** (see roadmap):
- Single-block only: pairs that span basic block boundaries are not elided.
- Not yet integrated into the production pipeline.

**Planned extension (Phase 4)**:
- Use `live_out[block]` to detect last-use releases.
- Extend pair search across block boundaries via BFS on the CFG.
- When the analysis proves a value is uniquely owned, downgrade `Release` to a direct free (skip the decrement check). This is the core Perceus optimization.

---

## Cycle handling

Lin uses **pure reference counting with no cycle detection**. Reference cycles between heap objects will leak memory. This is a documented design decision (ADR-024).

**Recommended practice**: Avoid creating long-lived reference cycles. If cycles are unavoidable (e.g., a graph algorithm), break them explicitly before the data becomes unreachable by setting a field to `Null`.

**Future options**:
- **Option B — Weak references**: Add a `Weak<T>` type that does not increment refcount. When the last strong reference is released, weak references become `Null`. Requires a tombstone flag in the header and a new type in `lin-check`.
- **Option C — ORC-style trial deletion**: When an object's refcount decrements but stays >0, add it to a "potential cycle root" set. Periodically run trial deletion (tentatively decrement reachable counts; objects that reach zero are cyclic garbage). This is how Nim ORC works. Gate behind a `--orc` flag.

---

## Roadmap

### Phase 1 — Runtime correctness (complete)

| Task | Status | Files |
|---|---|---|
| 1.1 Recursive array release | ✅ Done | `lin-runtime/src/array.rs` |
| 1.2 Recursive object release | ✅ Done | `lin-runtime/src/object.rs` |
| 1.3 Closure refcount + `lin_closure_release` | ✅ Done | `lin-runtime/src/memory.rs`, `lin-codegen/src/codegen.rs` |
| 1.4 Tactical TaggedVal box leak fix | ✅ Done | `lin-codegen/src/codegen.rs` |
| Option A: Document cycle limitation | ✅ Done | `docs/DECISIONS.md` ADR-024 |

### Phase 2 — Systematic RC emission in the codegen-direct backend (removed)

This phase tracked RC emission inside the **TypedAST → LLVM** backend, which inserted release calls by hand at consumption points and had a list of known gaps (`var` reassignment release, match-scrutinee release, …). **That backend has been removed** — the LinIR pipeline below (Phases 3–4) is now the sole lowering path, and ownership is decided systematically in `lower.rs` rather than patched into codegen consumption sites. The old gap table is gone because the code it described no longer exists; any remaining RC subtleties live in `lower.rs`'s scope-frame ownership model.

### Phase 3 — Wire LinIR into production (complete)

| Task | Status | Notes |
|---|---|---|
| RC insertion in `lower.rs` (pessimistic Retain/Release) | ✅ Done | `scope_owned` stack tracks owned temps; Release emitted at Block/function scope exits |
| `compile_module_from_ir` in `lin-codegen` | ✅ Done | Handles all `Instruction` variants, translates to LLVM using existing helpers |
| LinIR is the sole lowering path | ✅ Done | `lin-compile/src/lib.rs` always routes through `lower_module` → `elide_rc` → `compile_module_from_ir`; the former env-gated routing and the TypedAST-direct fallback are gone |

Key files: `lin-ir/src/lower.rs`, `lin-codegen/src/codegen/`, `lin-compile/src/lib.rs`

### Phase 4 — Cross-block RC elision (complete)

| Task | Status | Notes |
|---|---|---|
| Rename `_liveness` → `liveness` and use it | ✅ Done | Liveness drives last-use detection in cross-block path |
| BFS cross-block Retain/Release pair search | ✅ Done | Searches up to 8 successor blocks via CFG terminator edges |
| Clean-path check for intermediate blocks | ✅ Done | `block_is_clean_for` rejects blocks with calls/allocs/releases |
| Clean-path check for retain-block tail | ✅ Done | `path_has_no_interference` checks instrs after Retain to block end |
| Clean-path check for release-block prefix | ✅ Done | `path_has_no_interference` (sentinel=MAX) checks instrs before Release |
| Last-use liveness assertion in tests | ✅ Done | Test verifies `live_out[block1]` does not contain t0 |
| Unit tests: 4 new cross-block cases | ✅ Done | Clean path elide, call-in-retain-block keep, call-in-intermediate keep, last-use elide |

**What was implemented**: `elide_rc_fn` now builds a `block_index` map and, when a same-block Release is not found, does a BFS over CFG successors (capped at `BFS_BLOCK_LIMIT = 8`) using `find_paired_release_cross_block`. The path must be clean in all three segments: (1) the retain-block tail after the Retain, (2) every intermediate block traversed by BFS, and (3) the release-block prefix before the Release. The `Liveness` struct (previously unused as `_liveness`) is now computed and used; the last-use property (temp not in `live_out` of the release block) is tested explicitly.

**Not yet implemented**: The uniqueness/direct-free optimization (`FreeDirect` instruction) described in step 3c — deferred to Phase 5 or a separate PR.

Key files: `lin-ir/src/rc_elide.rs`, `lin-ir/src/liveness.rs`

### Phase 5 — Perceus reuse analysis / FBIP (optional, future)

When a uniquely-owned value is destroyed at the same point a same-shaped allocation occurs, reuse the freed memory instead of allocating fresh. Add `MakeReuse` / `AllocReuse` IR instructions and `lin_reuse_token` / `lin_alloc_with_reuse` runtime primitives. Primary benefit: `map`/`filter` chains that create many same-shaped intermediate arrays.

---

## Reference counting under threads (async / concurrency)

Lin's RC is **non-atomic** on the single-threaded hot path. Real OS-thread concurrency (spec
§24, ADR-028/044/045) keeps that hot path free by **never sharing ordinary mutable heap values
across threads** — three mechanisms cover every cross-thread case:

1. **Transfer by deep copy (Option C) — the default.** When a value crosses a thread boundary
   (a thunk's captured `val`s, or a transferable result handed back through a promise), it is
   **deep-copied** so each thread owns a private, disjoint object graph (`lin_transfer_clone`,
   `transfer_clone_env` in `transfer.rs`). Nothing is shared, so non-atomic RC stays sound. The
   boundary-crossing set is exactly the transferable types (JSON-shaped, acyclic — enforced by
   the checker), so the copy is total and bounded. The closure env carries a codegen-emitted
   **capture descriptor** (one `CAP_*` byte per slot, stored at the env's offset-0 word) so the
   runtime knows which env words are heap pointers to deep-copy; an env that captures a
   function/iterator (`CAP_OPAQUE`) is not transferable, so that thunk runs inline as a sound
   fallback.

2. **`Shared<T>` — opt-in shared *mutable* state (ADR-029).** An **atomic**-refcounted box
   (`SharedBox`, `shared.rs`) wrapping an `RwLock` over the inner value. Only the box's refcount
   is atomic; the inner graph keeps ordinary non-atomic RC because it is only ever reachable
   while a lock is held. Every value enters/leaves by deep copy, so no live reference escapes
   the lock. The transfer copy path *shares* a `Shared` box by an atomic bump (the nesting
   rule), never copies through it.

3. **`Frozen<T>` — opt-in shared *read-only* state (ADR-030).** `frozen(v)` deep-seals the graph
   **immortal** (saturated refcount), reusing the interned-string immortality trick generalized
   to a whole graph. The immortal guard on string/array/object retain/release makes RC a no-op
   on frozen nodes, so a read-only function's existing non-atomic RC runs correctly on a value
   shared by N threads — no recompilation, no lock, no atomics. Frozen ⇒ never freed (load-once
   data only). The transfer path shares frozen nodes by reference (zero-copy).

Catchable faults: a runtime fault inside an async thunk unwinds to the thread boundary and
becomes an `Error` (fault.rs / ADR-028); the faulting runtime functions and the thunk-call
transmutes are `extern "C-unwind"` and async-reachable Lin frames carry `uwtable`.

Atomic-RC-everywhere (Option A), dynamic shared-flag RC (Option D), and copy-on-write were
rejected (ADR-028 §2.3) — they tax the non-threaded hot path. **TSan** (ThreadSanitizer) is the
right tool for the RC-race class; ASan covers leaks/use-after-free across the transfer + box
machinery and is wired in CI.

---

## Testing approach

- Run `cargo test --workspace` after every phase to verify no regressions.
- Run with AddressSanitizer to detect leaks and use-after-free: `RUSTFLAGS="-Z sanitizer=address" cargo test --workspace` (requires nightly).
- Use `LIN_EMIT_IR=1` to inspect LLVM IR and verify retain/release instructions are present and well-placed.
- Integration tests for nested structures: `crates/lin/tests/integration.rs`.

---

## Reading list

- Reinking et al., "Perceus: Garbage Free Reference Counting with Reuse", PLDI 2021. — Foundation for the elision and reuse analysis passes.
- Nim ARC/ORC documentation — Model for destructor injection and trial-deletion cycle collection.
- `docs/DECISIONS.md` ADR-024 — Rationale for choosing RC over tracing GC.
- `lin-ir/src/rc_elide.rs` — Current elision pass implementation and unit tests.
- `lin-ir/src/liveness.rs` — Backward-dataflow liveness analysis used by the elision pass.
