# Memory Management in Lin

Lin uses **deterministic reference counting (RC)** for all heap-allocated values. There are no GC pauses, no background threads, and no programmer annotations required for the common case. RC is **non-atomic** on the single-threaded hot path; thread safety is achieved by sharing nothing across threads by default (see "Reference counting under threads"). This document describes the runtime layout, the compiler RC strategy, cycle handling, and the deferred work — as it exists today, not as a roadmap.

The runtime lives in `crates/lin-runtime/src/`; the compiler RC passes live in `crates/lin-ir/src/` and `crates/lin-codegen/src/codegen/rc.rs`.

---

## Heap-allocated representations

Every heap value carries a `u32` refcount as its **first field** (offset 0), so the generic `lin_rc_retain` can bump any of them. Scalars (`Int32`, `Int64`, `Float32`, `Float64`, `Bool`, `Null`) are stored unboxed as LLVM primitives and carry no refcount.

| Representation | Runtime struct | Tag | Releaser |
|---|---|---|---|
| `String` | `LinString` (`string.rs`) | `TAG_STR` (6) | `lin_string_release` |
| array / `T[]` | `LinArray` (`array.rs`) | `TAG_ARRAY` (8) | `lin_array_release` |
| open object / index-map | `LinMap` (`map.rs`) | `TAG_MAP` (20) | `lin_map_release` |
| closure | 48-byte struct (`memory.rs`) | `TAG_FUNCTION` (9) | `lin_closure_release` |
| sealed record (packed) | header + inline fields (`sealed.rs`) | `TAG_RECORD` (25) | `lin_sealed_release{,_self}` |
| unboxed sum node | header + inline payload (`sumnode.rs`) | `TAG_SUMNODE` (21) | `lin_sumnode_release{,_self}` |
| boxed union value | `TaggedVal` (`tagged.rs`) | wraps any tag | `lin_tagged_release` |

> **There is no `LinObject`.** Open objects (and typed index-signature containers, ADR-055) are backed by `LinMap`, a SwissTable. Any older reference to a `LinObject` struct or `lin_object_release` is obsolete — the equivalents are `LinMap` / `lin_map_release`. `TAG_OBJECT` (7) is **retired** (no producers; reserved).

### `LinString` (`string.rs`)

```rust
#[repr(C)]
pub struct LinString {
    pub refcount: u32,   // +0
    pub len: u32,        // +4
    pub hash: u64,       // +8   cached FNV-1a of data bytes; 0 = not yet computed
    pub data: [u8; 0],   // +16  inline UTF-8 bytes
}
```

The 16-byte header (refcount, len, **cached hash**, then inline data) is one allocation. The cached hash makes string-keyed map lookups cheap (compute once, reuse).

### `LinArray` (`array.rs`)

The header is fixed; the meaning of `elem_tag` selects the element storage:

```rust
#[repr(C)]
pub struct LinArray {
    pub refcount: u32,              // +0
    pub elem_tag: u8,              // +4   storage discriminant (see below)
    _pad3: [u8; 3],                // +5
    pub len: u64,                  // +8
    pub cap: u64,                  // +16
    pub data: *mut LinArrayElem,   // +24
    pub elem_stride: u64,          // +32
    pub elem_desc: *const u8,      // +40
    pub elem_named_desc: *const u8,// +48
}
```

`elem_tag` values:

| `elem_tag` | Storage | Element release |
|---|---|---|
| `0xFF` | **tagged** elements — each a 16-byte `LinArrayElem { tag:u8, _pad:[u8;7], payload:u64 }` (byte-identical to `TaggedVal`) | recurse per element on heap tags |
| a scalar tag (`TAG_INT32`, `TAG_FLOAT64`, `TAG_UINT8`, …) | **flat scalar** — raw `T` elements, no per-element box | none (no pointers) |
| `0xFE` (`SEALED_ARRAY_TAG`) | inline contiguous **sealed-record payloads** (header-less, `elem_stride` apart) | descriptor-walk each slot |
| `0xFD` (`SEALED_PTR_ARRAY_TAG`) | pointer-backed sealed-record elements | release each pointer |
| `0xFC` (`COLUMNAR_ARRAY_TAG`, `columnar.rs`) | struct-of-arrays columnar layout | release columns |

All variants share `lin_array_release`, which dispatches on `elem_tag` at drop time. Flat scalar arrays skip element recursion entirely.

### `LinMap` (`map.rs`)

A SwissTable-backed map; the runtime representation behind open objects and `TAG_MAP` containers.

```rust
#[repr(C)]
pub struct LinMap {
    pub refcount: u32,   // +0
    pub len: u32,        // +4   occupied slots
    pub cap: u32,        // +8   table size (power of two)
    pub key_kind: u32,   // +12  KEY_KIND_STRING (0) or KEY_KIND_INT (1)
    pub slots: *mut u8,  // +16  `cap` slots of slot_stride(value_kind) bytes
    pub order: *mut u64, // +24  insertion-order key list (preserves iteration order)
    pub cap_order: u32,  // +32  capacity of `order`
    pub value_kind: u32, // +36  VKIND_UNINIT / VKIND_MIXED / a real value tag (codegen-visible)
    pub ctrl: *mut u8,   // +40  `cap` SwissTable control bytes
}
```

`lin_map_release` at rc==0 releases all string keys (when `key_kind == KEY_KIND_STRING`) and all heap-typed values, then frees the `ctrl`, `slots`, and `order` allocations and the header.

### Closure (48 bytes, `memory.rs`)

There is no named `LinClosure` Rust struct; the layout is written by codegen (`make_closure_struct_desc_caps`) and documented in `lin_closure_release`:

```
Offset  Field               Type   Notes
0       refcount            u32    bumped on retain, freed at 0
4       _pad                u32    alignment
8       fn_ptr              ptr    LLVM function: (env_ptr, args...) -> ret
16      env_ptr             ptr    heap env struct, or null for non-capturing
24      env_size            u64    byte-size of env allocation; 0 when env_ptr is null
32      default_descriptor  ptr    static global (default-arg desc), or null; never freed here
40      capture_descriptor  ptr    static global (capture desc, ADR-041), or null; never freed here
```

All closures use this uniform 48-byte layout and the `fn_ptr(env_ptr, args...)` calling convention. The **env struct** is `{ u64 size @0, cap0 @8, cap1 @16, … }` — captured values sit at 8-byte slots starting at offset 8. The **capture descriptor** (a static global pointed to from closure offset 40) is `{ u32 count, u8 kinds[count] }` with one `CAP_*` byte per captured slot; when non-null, the closure *owns* one reference per owning capture, so `lin_closure_release` releases each before freeing the env and the 48-byte struct.

### Sealed records and sum nodes

Both are unboxed, descriptor-driven, refcount-first headers of **24 bytes** (`SEALED_HEADER` / `SUMNODE_HEADER`):

- **Sealed record** (`sealed.rs`, `TAG_RECORD`): `[ u32 refcount | u32 size | u64 desc_ptr | inline fields… ]`. `desc_ptr` is a static, codegen-emitted field descriptor (a count plus `KIND_*` bytes: `KIND_STRING`/`KIND_ARRAY`/`KIND_SEALED`/`KIND_MAP`/`KIND_SUMNODE_FIELD`), or null for a scalar-only record. `lin_sealed_release(ptr, size)` walks the descriptor, releases each heap field, then frees; `lin_sealed_release_self(ptr)` reads `size` from the header first.
- **Sum node** (`sumnode.rs`, `TAG_SUMNODE`): `[ u32 refcount | u32 size | u64 desc_ptr | u32 tag | u32 _pad | inline payload… ]` — an inline discriminant `tag` at offset 16. `lin_sumnode_release(ptr, size)` / `lin_sumnode_release_self(ptr)` release the active variant's heap fields via the descriptor, then free.

### `TaggedVal` box (`tagged.rs`)

Union-typed values (`AnyVal`, unresolved `TypeVar`, and any value flowing through a union-shaped slot) are heap-boxed:

```rust
#[repr(C)]
pub struct TaggedVal { pub tag: u8, pub _pad: [u8; 7], pub payload: u64 }  // 16 bytes
```

`payload` is either an inline scalar or a pointer to one of the heap representations above. `lin_tagged_release` releases the inner payload by tag, then frees the 16-byte box.

### Opaque handles

Some tags name opaque runtime handles, not ordinary RC graphs: `TAG_PROMISE` (16) and `TAG_HANDLE` (17) are non-refcounted; `TAG_STREAM` (19), `TAG_BIGNUM` (22), `TAG_DECIMAL` (23), `TAG_TAR_ENTRY` (24) carry their own box refcounts; `TAG_SHARED` (18) is **atomic**-refcounted (see threads section). These are released through `lin_tagged_release`'s tag-specific arms.

---

## Reference counting mechanics

| Function | Behaviour |
|---|---|
| `lin_rc_retain(ptr)` | Increments `*(ptr as *u32)` — generic, works on any RC representation. No-op when the count is `>= IMMORTAL_RC`. |
| `lin_rc_release(ptr)` | Generic decrement (guarded by `IMMORTAL_RC`). |
| `lin_string_release(s)` | Decrements; frees the single allocation at zero. |
| `lin_array_release(arr)` | Decrements; at zero, **recursively releases** heap-typed elements (tags `TAG_STR`, `TAG_ARRAY`, `TAG_MAP`, `TAG_RECORD`, `TAG_SUMNODE`, `TAG_FUNCTION`), or descriptor-walks sealed/columnar layouts, then frees header + data. Flat scalar arrays skip recursion. |
| `lin_map_release(map)` | Decrements; at zero, releases string keys and heap-typed values, then frees `ctrl`/`slots`/`order` + header. |
| `lin_closure_release(ptr)` | Decrements; at zero, releases owning captures via the capture descriptor (offset 40), then frees the env (size at offset 24) and the 48-byte struct. |
| `lin_sealed_release{,_self}` / `lin_sumnode_release{,_self}` | Decrement; at zero, descriptor-walk the heap fields of the (active variant of the) record, then free. |
| `lin_tagged_release(p)` | Releases the inner heap value by tag, then frees the 16-byte `TaggedVal` box. |

The recursive release in arrays, maps, sealed records, and sum nodes means **nested structures are freed correctly without compiler assistance** — the compiler only has to balance the *root* reference.

### `IMMORTAL_RC` — the immortality sentinel

```rust
pub const IMMORTAL_RC: u32 = 0x8000_0000;   // string.rs
```

A node whose refcount is `>= IMMORTAL_RC` is **immortal**: every retain and release path (`lin_rc_retain`/`lin_rc_release`, `lin_string_release`, `lin_array_release`, `lin_map_release`, `lin_sealed_release`, `lin_sumnode_release`, and the direct bumps in `retain_tagged_payload`) checks the sentinel first and becomes a no-op. This is used for interned string literals and for `frozen` graphs (see threads section) — the count is never written, so concurrent reads of it are race-free.

---

## Compiler RC strategy

RC is inserted, optimised, and lowered entirely through the **LinIR pipeline** — the sole lowering path. (The earlier TypedAST-direct backend, which inserted release calls by hand at consumption points, has been removed; do not look for it.) The production pipeline in `lin-compile/src/lib.rs` is:

```
TypedModule
  → lower_module_with_imports (lin-ir/src/lower) : scope-frame ownership inserts
                                                   pessimistic Retain/Release/CloneBox/FreeBoxShell
  → ownership_verify::infer_conventions          : per-param/per-return ownership annotation
  → rc_elide::elide_rc (lin-ir/src/rc_elide)     : liveness-driven elision of redundant pairs
  → compile_module_from_ir (lin-codegen)         : each Retain/Release → a repr-dispatched runtime call
```

All four stages run on every build; the elision pass is **always** invoked (`rc_elide::elide_rc(&mut ir_module)` in `lib.rs`), not gated or optional.

### Lowering (`lin-ir/src/lower`)

Lowering inserts RC **pessimistically** — `elide_rc` removes provably-redundant pairs afterward. Ownership is tracked by a stack of **scope frames** (`scope_owned: Vec<Vec<(Temp, Type)>>`): every freshly-owned heap/union temp is `register_owned`-ed at its origin and released when its scope pops (`pop_scope_releasing_keep`, which transfers — rather than releases — temps named in the `keep` set so a returned value's obligation moves to the caller). Container inserts, call-argument boxing, var/global stores, index/field projections, and tail calls each adjust ownership at their site. The model: each owned reference has exactly one releasing owner; transfers move the obligation; borrows net to zero.

The **tail-call path** is special: scope-exit releases emitted *after* a `TailCall` would land in the dead `tco_post` block and never run (leaking once per iteration), so owned temps are drained on the *live* block before the back-edge (`release_owned_for_tail_call`), distinguishing pass-through args (release the registration) from transferring args (keep the transferred +1).

**RC-related IR instructions** (`lin-ir/src/ir.rs`):

| Instruction | Meaning |
|---|---|
| `Retain { val, ty }` | increment refcount |
| `Release { val, ty }` | decrement; free at zero |
| `CloneBox { dst, src, ty }` | clone a boxed `TaggedVal*` (fresh +1 box copying tag+payload, retaining the inner heap value) |
| `FreeBoxShell { val }` | free *only* the 16-byte box shell, not the inner payload (transient boxes) |
| `FreeBoxShellIfDistinct { val, other }` | free the box shell only if `val != other` (loop element box that may alias a callback return) |
| `ReleaseIfDistinct { val, other }` | full release of a `TaggedVal*` only if distinct from `other` |
| `ReleaseRawIfDistinct { val, other, ty }` | type-aware raw (non-tagged) release guarded by pointer inequality (e.g. a `reduce` accumulator identity guard) |

### Codegen (`lin-codegen/src/codegen/rc.rs`)

`Retain`/`Release` are lowered to runtime calls dispatched on the value's **physical representation** (`emit_release_repr`), not just its static type — so a sealed packed record, a packed sealed array (`0xFE`), a columnar array (`0xFC`), a `SumNode`, a nullable-record, a plain map, and a boxed union each get the correct releaser. Non-packed reprs fall back to the type-based `emit_release` (string → `lin_string_release`, array → `lin_array_release`, sealed-scalar object → sealed releaser, other object/map → `lin_map_release`, closure → `lin_closure_release`, union/TypeVar/stream/promise/opaque → `lin_tagged_release`).

---

## Perceus elision (RC elision pass)

`lin-ir/src/rc_elide.rs` implements a conservative approximation of the Perceus algorithm (Reinking et al., PLDI 2021), backed by the backward-dataflow liveness in `lin-ir/src/liveness.rs` (`Liveness::compute`, per-instruction live sets — actively used, not dead code).

For each `Retain { val }` the pass searches for a matching `Release { val }` and removes both when the value is provably never shared between them:

1. **Same-block.** Pair a `Retain` with the first unclaimed `Release` of the same temp later in the block; require `path_has_no_interference` between them (no calls, heap allocations, aliasing releases, or escapes) and that the Release is the temp's **last use** (`release_is_last_use` — the temp is not in `live_out` after it).
2. **Cross-block.** When no same-block release exists, `find_paired_release_cross_block` walks the **immediate post-dominator (idom) chain** from the retain block to fixpoint (every block on the chain post-dominates the origin by construction; there is no fixed block cap). It then verifies the path is clean in all three segments — the retain-block tail after the `Retain`, every intermediate block (`block_is_clean_for`), and the release-block prefix before the `Release` — checks `post_dominates(release_block, retain_block)` (the Release is reached on every path), and applies the same last-use liveness gate.

**Deferred (not implemented):**
- **Uniqueness / direct-free** (a `FreeDirect` instruction that skips the decrement check when a value is proven uniquely owned) — the core Perceus optimisation, not yet present.
- **Perceus reuse / FBIP** (`MakeReuse`/`AllocReuse` IR + `lin_reuse_token`/`lin_alloc_with_reuse` runtime, reusing a uniquely-owned destroyed value's memory for a same-shaped allocation — e.g. `map`/`filter` chains).

---

## Cycle handling

Lin uses **pure reference counting with no cycle detection**. Reference cycles between heap objects leak memory. This is a documented design decision (ADR-024). `Shared<T>` (below) is the one ordinary way to create a cycle, since it permits two boxes to reference each other; ordinary records/arrays/maps are built bottom-up and cannot form a cycle without a `Shared` (or a `var` mutated to point back).

**Recommended practice:** avoid long-lived cycles; if unavoidable (e.g. a graph), break them explicitly before the data becomes unreachable (set the back-edge field to `Null`).

**Future options (not implemented):**
- **Weak references** — a `Weak<T>` type that does not increment the count and reads as `Null` once the last strong reference is gone (needs a tombstone flag in the header and a new type in `lin-check`).
- **ORC-style trial deletion** — track potential cycle roots and periodically trial-delete (à la Nim ORC), gated behind a flag.

---

## Reference counting under threads (async / concurrency)

Lin's RC is **non-atomic** on the single-threaded hot path — the refcounts are plain `u32` and retain/release are plain `+= 1` / `-= 1` (e.g. `string.rs`, `array.rs`, `memory.rs`). Real OS-thread concurrency (spec §24, ADR-028/029/030/045) keeps that hot path free by **never sharing ordinary mutable heap values across threads**. Three mechanisms cover every cross-thread case:

1. **Transfer by deep copy (Option C) — the default.** When a value crosses a thread boundary (a thunk's captured `val`s, or a transferable result handed back through a promise) it is **deep-copied** so each thread owns a private, disjoint object graph (`lin_transfer_clone`, `transfer_clone_env` in `transfer.rs`). Nothing is shared, so non-atomic RC stays sound; the boundary-crossing set is exactly the transferable types (JSON-shaped, acyclic — enforced by the checker), so the copy is total and bounded. The closure env carries a codegen-emitted **capture descriptor** (one `CAP_*` byte per slot — `CAP_NONE`/`CAP_STR`/`CAP_ARRAY`/`CAP_OBJECT`/`CAP_CLOSURE`/`CAP_TAGGED`/`CAP_MOVE`/`CAP_SEALED`) so the runtime knows which env words are heap pointers to deep-copy. A thunk whose env transitively captures a non-transferable value (a `CAP_CLOSURE` whose own env is not transferable, or a moved resource) fails the `env_is_transferable` check and runs **inline** on the calling thread — a sound, no-parallelism fallback (still inside the fault boundary).

2. **`Shared<T>` — opt-in shared *mutable* state (ADR-029).** A `SharedBox` (`shared.rs`) of `{ rc: AtomicU32, inner: RwLock<…> }`: only the box's refcount is **atomic**, and the inner graph keeps ordinary non-atomic RC because it is only ever reachable while a lock is held. Every value enters by deep copy (`shared`/`set`) and leaves by deep copy (`get`/`withLock`), so no live reference escapes the lock. The transfer copy path **shares** a `Shared` box by an atomic refcount bump (the nesting rule), never copies through it.

3. **`Frozen` — opt-in shared *read-only* state (ADR-030).** `lin_freeze(v)` (`frozen.rs`) deep-seals the graph **immortal** (saturates every node's refcount to `IMMORTAL_RC`, recursively over strings/arrays/maps/sealed records/sum nodes), reusing the interned-string immortality trick generalised to a whole graph, and returns `v` with its plain type. The `IMMORTAL_RC` guard makes retain/release no-ops on frozen nodes, so a read-only function's existing non-atomic RC runs correctly on a value shared by N threads — no recompilation, no lock, no atomics. The transfer path shares frozen nodes **by reference** (zero-copy). Immortal ⇒ never freed: `frozen` is for load-once, program-lifetime data; freezing in a loop leaks. (Compile-time read-only enforcement — the `Frozen<T>` type + mutation-inference coercion — is deferred; mutating a frozen value today is a silent no-op, not a diagnosed error.)

**Catchable faults.** A runtime fault (`runtime_fault` in `fault.rs`) branches on a thread-local async-boundary depth (`ASYNC_DEPTH`): inside an async boundary it `panic!`s and unwinds to the boundary's `catch_unwind` (`with_async_boundary`), becoming an `Error` at `await` (spec §24.2.2); outside, it keeps the top-level `process::exit(1)` (spec §20.1). The faulting runtime functions and the thunk-call transmutes are `extern "C-unwind"`, and codegen drops `nounwind` from user functions whenever the program uses async (`Codegen::module_uses_async`) so the unwind can cross back through Lin frames. See ADR-028.

Atomic-RC-everywhere (Option A), dynamic shared-flag RC (Option D), and copy-on-write were rejected (ADR-028) — they tax the non-threaded hot path. **TSan** is the right tool for the RC-race class; **ASan** covers leaks/use-after-free across the transfer + box machinery and is wired in CI.

---

## Testing approach

- Run `cargo test --workspace` after changes (build first — the integration tests invoke `target/debug/lin` as a subprocess, so a stale binary causes spurious failures).
- Run with AddressSanitizer to detect leaks and use-after-free: `RUSTFLAGS="-Z sanitizer=address" cargo test --workspace` (requires nightly).
- Use `LIN_EMIT_IR=1` to inspect the emitted LLVM IR and confirm retain/release placement.
- Integration tests for nested structures and RC edge cases: `crates/lin/tests/integration.rs`; the elision pass has unit tests in `rc_elide.rs`.

---

## Reading list

- Reinking et al., "Perceus: Garbage Free Reference Counting with Reuse", PLDI 2021 — foundation for the elision (and the deferred reuse) pass.
- Nim ARC/ORC documentation — model for destructor injection and trial-deletion cycle collection.
- `docs/DECISIONS.md` — ADR-024 (RC over tracing GC), ADR-028/029/030 (threads: copy-by-default, `Shared<T>`, `Frozen`), ADR-041 (closures own their captures; capture descriptors), ADR-055 (typed index-signature maps).
- `lin-ir/src/rc_elide.rs` / `lin-ir/src/liveness.rs` — the elision pass and the liveness analysis it uses.
- `crates/lin-runtime/src/` — `string.rs`, `array.rs`, `map.rs`, `sealed.rs`, `sumnode.rs`, `tagged.rs`, `memory.rs`, `transfer.rs`, `shared.rs`, `frozen.rs`, `fault.rs`.
