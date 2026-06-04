# Proposal: O(1) lookup for large `Json` objects (RAPTOR issue #4b)

Status: proposal (not yet implemented). Author investigation: 2026-06-04, branch
`fix/lin-json-map-semantics`.

## Problem

`Json` objects are association lists. `crates/lin-runtime/src/object.rs` implements
`lin_object_get`, `lin_object_set`, `lin_object_has`, `lin_object_merge`,
`lin_object_copy_except`, and `lin_object_eq` as **linear scans** over the entries buffer
(`for i in 0..len { if key_eq(entry[i].key, key) … }`). Every key access is O(n); building a
map of N distinct keys by repeated `set`/`get` is O(n²).

This is invisible for record-shaped objects (a handful of fields) but catastrophic for
dictionary-shaped objects. Porting RAPTOR, keying ~16k distinct `routeId`s while indexing
240k trips made the index-build phase ~145s, versus ~0.5s in hashed-map languages. The port
worked around it by avoiding big object maps entirely (contiguous-run grouping + sorted-array
binary search) — i.e. the language's only dictionary type was unusable at scale.

## What the layout looks like today (and what is coupled to it)

`LinObject` (`object.rs`):

```
offset  field
  0      refcount: u32
  4      len:      u32
  8      cap:      u32
 12      flags:    u32          // bit 0 = FLAG_INLINE
 16      entries:  *mut LinObjectEntry
```

`LinObjectEntry` is 24 bytes: `key: *mut LinString` @0, `value.tag: u8` @8,
`value.payload: u64` @16. Fresh small objects use a SINGLE-ALLOCATION layout: the entries
buffer sits immediately after the header in the same block (`FLAG_INLINE`); the first grow
migrates entries to a separate heap buffer (header never moves).

**Coupling that constrains any change** — codegen does NOT treat `LinObject` as fully opaque:

- `crates/lin-codegen/src/codegen/mod.rs` (~line 1230, the inline `MakeObject` fast path)
  writes object-literal fields by **direct GEP at hardcoded byte offsets**: it loads
  `entries` from `obj+16`, writes each entry at stride 24 (`key@base`, `tag@base+8`,
  `payload@base+16`), and writes `len` to `obj+4`. Crucially this path **bypasses
  `lin_object_set` entirely** for spread-free literals (it relies on the literal's keys being
  pre-deduplicated by codegen).
- Everything else (`obj[k]`, `obj[k]=v`, `keys`, `merge`, `has`, equality, `groupBy`) goes
  through the runtime FFI functions and treats the pointer as opaque.

So: the **header offsets 0/4/8/16 and the 24-byte entry stride are an ABI contract** with
codegen. Any change that keeps those fixed and only appends/adds state is codegen-safe; any
change that reorders them must update the codegen inline path in lockstep.

## Options considered

### (a) Hash side-index that activates past a size threshold — RECOMMENDED

Keep the assoc-list entries exactly as they are (preserving insertion order, the
single-allocation small-object optimization, and the codegen ABI). Add an **optional** open-
addressing hash index that maps `hash(key) → entry slot index`, built lazily once an object
grows past a threshold (e.g. `len >= 16`). Small objects keep their current layout and code
paths untouched; only big objects pay for (and benefit from) the index.

Sketch of the layout change — append, never reorder:

```rust
#[repr(C)]
pub struct LinObject {
    pub refcount: u32,   // @0   unchanged
    pub len: u32,        // @4   unchanged (codegen writes this)
    pub cap: u32,        // @8   unchanged
    flags: u32,          // @12  unchanged
    pub entries: *mut LinObjectEntry, // @16 unchanged (codegen reads this)
    // --- NEW, all at offset >= 24, which codegen never touches ---
    index: *mut u32,     // @24  open-addressing table of (entry_slot+1); null = no index yet
    index_cap: u32,      // @32  power-of-two table size, or 0
    index_dirty: u32,    // @36  set when entries changed without index maintenance
}
```

Behaviour:

- `lin_object_get` / `lin_object_has`: if `index` is null and `len >= THRESHOLD`, build the
  index (one O(n) pass), then probe in O(1) average. Below the threshold, keep the linear
  scan (it's faster than hashing for tiny N and avoids any allocation).
- `lin_object_set`: on the *append* branch, insert into the index too (if present); on grow,
  the index keeps pointing at slot indices (entries move within the buffer but their slot
  *indices* are stable on append/realloc — only the buffer base moves), so the index survives
  a realloc. On the *overwrite* branch nothing changes (same slot).
- The codegen inline-literal path bypasses `lin_object_set`, so a freshly-built large literal
  has `index == null`. That is fine: the FIRST `get`/`has` lazily builds it. We must NOT
  require the index to be maintained by the inline path — hence lazy build keyed off
  `index == null || index_dirty`.
- `lin_object_merge`, `lin_object_copy_except`, deep-copy (`object_push_owned`): set
  `index_dirty = 1` (or rebuild) so the next lookup refreshes it.
- `lin_object_release`: free the `index` table (if non-null) before freeing the object.
- `lin_object_keys` / `values` / `entries` / `eq`: unchanged — they iterate `entries`
  directly and don't need the index (order-independence of `eq` is preserved by still
  scanning, or optionally probing the index on the other side).

Hash: reuse the key bytes. A small FxHash/FNV over `(*key).data[..len]` is enough; keys are
`LinString*` and we already have `lin_string_key_eq` for confirmation on probe collisions.

**Risk/scope.** Moderate, NOT trivial:
- Touches every mutator in `object.rs`, the file the project's own notes flag as the recurring
  UAF/double-free hotspot (`project_rc_ownership_invariants`). Index maintenance is not
  refcounted, so it's lower-risk than the value paths, but a stale/invalid slot index that
  outlives a shrink/rebuild would silently return the wrong value — a *correctness* hazard,
  exactly the class #4 is trying to remove.
- Needs ASan coverage (the repo notes `cargo test` won't catch the RC/pointer bugs here) plus
  fuzz-style tests: build N-key objects across the threshold, interleave set/overwrite/merge/
  copy_except/delete-via-rebuild, and assert get/has/keys all agree with a linear oracle.
- The lazy-build trigger interacts with the codegen inline path; that path must be re-audited
  to confirm it never reads past offset 24 (it currently does not) so appending fields is
  safe without a codegen change.

This is a **well-contained, opt-in** change (semantics and ordering identical; small objects
byte-for-byte unchanged), but it is **not a one-sitting, obviously-safe** change given the
file's history and the need for ASan/fuzz validation. See "Decision" below.

### (b) A dedicated `Map<K,V>` stdlib/runtime type

Introduce a first-class hashed map distinct from `Json` objects: a new runtime container
(`lin_map_*`), a `Map<K, V>` type in the checker, and `std/map` wrappers (`Map.new`, `get`,
`set`, `has`, `delete`, `keys`, `size`). This is the "honest" fix — dictionary use gets a type
that is *designed* for O(1), and `Json` objects stay record-shaped (which is what their assoc-
list + single-alloc layout is optimized for).

**Risk/scope.** Large but lower-coupling-risk: it does not touch the existing `object.rs`
mutators or the codegen object ABI at all (purely additive). But it is a real language-surface
feature: new `TypeExpr`/`Type` variant, checker inference (`is`/equality/`for`/destructuring
interactions), codegen for a new boxed container tag, runtime container + RC, stdlib module +
docs (`STDLIB.md`) + examples + tests. It also leaves a discoverability footgun: users reach
for `{}` first and only learn about `Map` after hitting the O(n²) wall (the same trap #4
describes for `sort` vs a missing `sortStable`).

## Recommendation

**Adopt option (a)** — a lazy hash side-index gated by a size threshold — as the primary fix.
It directly removes the O(n²) wall for the existing, discoverable `{}` type with zero surface-
language change, no change to small-object performance, and no change to the codegen ABI
(fields are appended past offset 24). It is the smallest change that makes the language's only
dictionary type usable at scale.

Option (b) is a reasonable *additional* feature for users who want an explicit hashed map with
non-string keys, but it does not fix the common case (people will keep using `{}`), so it
should not be the first move.

## Decision for THIS change set

Per the issue-fix brief, option (a) is implemented **only if** it is a small, safe, fully
test-covered change. After investigation it is **not** small-and-safe enough to land
confidently in this pass:

1. It touches every mutator in the repo's designated UAF/correctness hotspot file, and a
   wrong slot index is a *silent wrong result* — the exact failure mode #4 exists to kill.
2. Correctness requires ASan + interleaving/fuzz tests that the standard `cargo test` does not
   run, so I cannot prove it safe within this change set's gate.
3. The lazy-build trigger is entangled with codegen's inline-literal `MakeObject` path (which
   bypasses `lin_object_set`); getting that interaction wrong reintroduces O(n²) silently or
   returns stale slots.

It is therefore left as this written proposal with a concrete, low-ABI-risk design, to be
implemented as its own worktree with ASan/fuzz coverage. (#5, the dynamic-`Json`-arithmetic
correctness divergence in the same issue cluster, IS fixed in this change set — see the branch
commit / `crates/lin-runtime/src/tagged.rs` `lin_tagged_arith`.)

## Related, out of scope here

#4a (no stable O(n log n) `sortStable` in `std/array`) is a separate, independent stdlib gap
called out in the same RAPTOR issue. It is not addressed by this proposal.
