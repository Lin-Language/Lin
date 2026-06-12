# Project: Actually Improve Performance — the representation reset

**Status:** proposal / design — not yet started.
**Author:** drafted with Claude, 2026-06-12, from the post-RAPTOR performance retrospective.
**Prerequisite read:** `docs/PERFORMANCE.md` (§2 typed-vs-Json RAPTOR), `docs/DECISIONS.md`
(ADR-057 sealed records, ADR-062 representation inference, ADR-064 SumNode), `docs/SPECIFICATION.md`
§5.9 (structural typing + sealed records).

---

## 0. One-paragraph thesis

Lin is slower than Go not because of a fundamental language tradeoff, and not (mostly) because of
backend immaturity, but because of **one early representation decision**: typed records were built
*on top of* the JSON runtime object (`LinObject` — a heap-allocated, refcounted, string-keyed,
shareable thing). From that single choice everything followed: records became *reference types* by
accident, the compiler grew a flow-sensitive representation-inference pass to *claw back* struct
speed occurrence-by-occurrence, and a multi-year effort (sealing, ADR-062, the whole "path-9" saga)
went into optimizing *back toward* the value semantics the specification wanted all along. This
document proposes the inversion: **records are value types with a packed struct as their sole
representation; `Json` is dissolved into a dynamic top type (`Any`) plus ordinary hashmaps; the
boxed string-keyed object (`LinObject`) ceases to exist.** Making typed code fast then stops being a
special project and becomes a *consequence* of three clean lines — types, hashmaps, and a dynamic
top type — that the current design blurs.

---

## 1. Why we are here (the diagnosis)

### 1.1 What the measurements actually say

- **The typed scalar core is already at or above systems-language parity.** The `records`
  cross-language benchmark (sealed all-scalar structs, constant-offset field access) is **Lin 200 ms
  vs Rust 224 ms vs Go 624 ms** — Lin wins. The compilation model is not the problem.
- **Eager combinator chains beat Rust ~4×** (fused to a single zero-allocation loop). Not the problem.
- **A tracing GC would not help.** Measured (`LIN_NO_RC` ceiling): deleting *all* allocation + RC
  recovers ~0% on every workload, including RAPTOR's textbook-GC-bait retention profile. No workload
  is allocation-bound. The cost is *work per access*, not allocate/reclaim. RC stays.
- **The gap is concentrated at the typed-record representation boundary.** Fully-typed RAPTOR runs
  **1.96×** the `Json` port (after this session's de-materialization work; 2.33× before). The residual
  splits into: (a) reading heap-field records out of maps/arrays/unions — *per-access materialization*;
  (b) constructing/regrouping record collections — *copy cost*; (c) generic closure call boundaries —
  *box/unbox*. (a) and (c) are implementation gaps; (b) is the value-array copy that today's design
  makes unavoidable *because* records are reference-shared. See `docs/PERFORMANCE.md` §2.

### 1.2 The root cause

The runtime grew up around `LinObject`: a boxed, refcounted, **string-keyed** object — the carrier
for JSON. Typed records were implemented *as* `LinObject`s rather than *beside* them as structs. Two
consequences fell out, neither of them an intended language design decision:

1. **Records became reference types.** Two bindings can hold the same `LinObject` pointer, so mutation
   through one is observable through the other (`var b = a; a["k"] = v` → `b` sees it). Nobody
   specified "records are mutable shareable objects." It is the shadow of a shared pointer.

2. **"Json object" conflated two different things.** A *record* (fixed, statically-known fields →
   wants a struct) and a *hashmap* (dynamic string keys → wants a dictionary) were welded into one
   string-keyed representation. Field access on it is an association-list / hashed key lookup and an
   LLVM optimization barrier, even when the field set is statically known.

The specification actually leans the *other* way. §5.9.1 mandates a **non-mutating projection**:
when a value flows into a slot of named record type `T`, it is **copied** into a fresh sealed value
and the original is untouched. That is value-copy semantics at the type boundary. The current
implementation is therefore *internally inconsistent with its own spec*: it copies on projection
(value) but aliases on same-type assignment (reference). That inconsistency is the fingerprint of
"we reused the object that was already there," not of a considered model.

The representation-inference pass (ADR-062, `lin-ir/src/repr.rs`) exists solely to *recover* struct
speed from this boxed default — deciding, occurrence by occurrence, "packed when freshly constructed,
boxed when read back from a slot." It is the single largest source of implementation complexity and
the origin of every "path-9" dead end. It is a workaround for the wrong default.

---

## 2. The target model

There are exactly these kinds of values. Nothing else. In particular there is **no boxed
string-keyed object**.

| Kind | Type form | Representation | Field/elem access |
|------|-----------|----------------|-------------------|
| Scalar | `Int32`/`Float64`/`Bool`/… | inline machine value | n/a |
| String | `String` | refcounted byte buffer (handle) | n/a |
| Array | `T[]` | **contiguous buffer of packed `T`** | const-offset index |
| Hashmap | `{ String: T }` | hashed `LinMap`, values inline/owned | O(1) hash lookup |
| Record | `type P = {…}` / anon structural | **packed value struct** | **constant-offset load** |
| Union | `A \| B`, `T \| Null` | tag word + payload (inline or owned ptr) | `match … is T` → read payload at `T`'s layout |
| Any | `Any` (née `Json`) | recursive tagged union; see §2.4 | dispatch on tag |
| Opaque handle | `Function`, `Iterator`, `Promise`, `Stream`, `Shared`, `Frozen`, `TarEntry` | nominal runtime handle | n/a |

The three lines the current design blurs, drawn sharply:

- **Types** (records) — fixed fields, value semantics, struct layout, constant-offset access.
- **Hashmaps** (`{ String: T }`) — dynamic string keys, a real dictionary, O(1) lookup.
- **A dynamic top type** (`Any`) — "I don't know the shape; dispatch at runtime and pay for it."

### 2.1 Records are value types (the load-bearing semantic decision)

- Assignment, parameter passing, and return are **by value**. There is no observable aliasing.
  `val b = a` is a logically-independent value; `var b = a; a["f"] = v` does **not** affect `b`.
- Mutation happens only through `var` and only in place (a `val` is immutable). Because there is no
  aliasing, an owned record is always uniquely owned at its mutation site, so in-place write is
  always sound — no copy-on-write check needed.
- This is what §5.9.1's projection already implies; we are making it **total** rather than
  only-at-type-boundaries.
- Shared mutable state across scopes/threads is **explicit**: `Shared<T>` (already exists). Implicit
  accidental sharing is removed — which is a correctness improvement, not only a performance one.

### 2.2 Records have one representation: packed

A sealed record `T` is laid out exactly as §5.9.1 already describes:

- A small header (descriptor pointer + refcount).
- Scalar fields **inline** at their natural offset.
- Heap fields (`String`, array, map, nested record) as **8-byte owned pointer slots** with per-field
  retain/release.
- Field access is **always** a constant-offset load. There is no boxed shadow and no "read from a
  slot makes it boxed" arm. The representation is the same whether the record was freshly constructed
  or read out of an array/map/union.
- (Optional later optimization: inline *small* records into registers and pass them unboxed, like Go
  small structs. Not required for the model; start uniform.)

### 2.3 Arrays and maps hold records inline

- `T[]` is a contiguous buffer of packed `T` (Go's `[]T`), **not** an array of boxed pointers. This
  is the "arrays of heap-field records currently stay boxed" limitation that §5.9.1 explicitly flags
  as a *current implementation* fact — it disappears here. `arr[i]["f"]` is offset-plus-offset;
  `arr[i]["f"] = v` writes in place; `push` moves or copies the element in.
- `{ String: T }` stores packed `T` values inline (or owned pointers for large `T`).
- The existing flat scalar arrays (`lin_flat_array_*`) are the same idea already proven for scalars;
  this generalizes it to records, *unifying* two code paths rather than adding one.

### 2.4 `Any` is a type, not a representation (the `Json` dissolution)

`Json` is renamed and re-founded as **`Any`** — the dynamic top type. It is a recursive union:

```
Any  =  Null | Bool | Int* | Float* | String | Any[] | { String: Any } | <opaque handle>
```

- Its representation is the **tagged-union machinery you already have** (the tagged value / SumNode
  family, ADR-064). It is **not** a bespoke object.
- A *"JSON object"* is therefore just a `{ String: Any }` **hashmap**. There is no third thing.
- Index/field access on an `Any` dispatches on the tag: if the tag says hashmap, do a hash lookup; if
  it says array, index it; otherwise the safe-access rule yields `Null`. Appropriately dynamic, and
  honestly slow — which is fine, because that is what `Any` is *for*.
- The boundary, and the only place dynamic↔typed conversion happens:
  - **`T` → `Any`**: box. Start simple — project the record to a `{ String: Any }` hashmap (you have
    lost the static type at the dynamic boundary anyway). Later optimization: carry a packed record +
    descriptor and fast-path the round trip.
  - **`Any` → `T`**: a validating projection / `fromJson`-style copy (already specified).
- Rename rationale: `Any` reads as exactly what it is and stops implying a special JSON runtime
  object. JSON becomes purely a **wire format** — parsed into records/hashmaps/`Any`, serialized out
  of them — never a thing resident in memory.

### 2.5 The wrinkle: iteration order

The current "Json object" quietly provides **insertion-ordered** key iteration, and a little code
leans on it. Concretely: RAPTOR's `getQueue` keeps `Json` *specifically* because the within-round
tie-break between equal-arrival trips depends on key insertion order; a hash-ordered map changed
which trip a journey boarded and broke the cross-language digest (documented in
`benchmarks/compare/raptor/lin-typed/queueFactory.lin`).

A `{ String: T }` hashmap iterates in hash order. So when the Json object is dissolved, the ordering
guarantee must move somewhere **explicit**: either an *ordered-map* container variant (a linked
hashmap that preserves insertion order) for the few places that need it, or those places switch to a
list of `(key, value)` pairs. The right outcome is that ordering becomes an explicit property of an
explicit container, not a hidden affordance — but it is a conscious migration, not a free swap.

---

## 3. What gets deleted

This is a *simplifying* rewrite. The net line count goes **down**, and the mental model gets smaller.

- `lin-runtime/src/object.rs` and `lin_object_get`'s string-keyed scan — **gone**. No value exists on
  which you do a string-keyed lookup over an unknown field set; dynamic access is a hashmap O(1)
  lookup or an `Any` tag dispatch.
- The reconciliation arms of `lin-ir/src/repr.rs` (the flow-sensitive "packed-or-boxed" oracle/verify
  logic, the ADR-062 §H4/H5 machinery). What remains is a *layout calculator* (compute offsets;
  choose inline vs owned-pointer by size).
- `BoxKeepPacked` and the keep-packed-across-boundary machinery — there is no boundary to keep packed
  across; there is one representation.
- The boxed-shadow paths in `lin-codegen/src/codegen/boxing.rs`, the per-access materialize in
  `data.rs`, and `sealed.rs`'s rebuild-from-boxed.
- Essentially the entire "path-9" problem space, because it was the symptom of having two
  representations for one thing.

---

## 4. What we are explicitly NOT changing (non-goals)

- **Memory management stays Perceus-style RC.** Measured not alloc-bound; a tracing GC buys ~0%. RC
  is, in fact, the *enabling* technology here (§5.4).
- **Concurrency stays share-nothing** (deep-copy on transfer, `Shared`/`Frozen`, worker-owned state).
  This is *already* value semantics; the proposal removes an inconsistency rather than adding one.
- **Eager combinator fusion stays.** Already beats Rust; untouched.
- **The surface syntax is unchanged.** Braces still mean a record when the field set is statically
  known and a hashmap when the context type is `{ String: T }`. The syntax was never the issue.
- **No new general-purpose mutability/aliasing feature.** If you want sharing, you use `Shared<T>`.

---

## 5. Design details that need to be right

### 5.1 Record ↔ `Any` boundary cost

Boxing a record to `Any` by converting to a `{ String: Any }` hashmap is a real cost paid at the
moment you go dynamic. That is acceptable (it is the escape hatch) but means we should:
- Keep the typed path wide so values rarely *need* to become `Any` (the whole point).
- Plan the later optimization where `Any` can carry a packed record + descriptor and the `Any → T`
  projection fast-paths when the descriptor already matches `T`.

### 5.2 Unions and `match` narrowing the *value*

Once a record has a single representation, `match x is T => …` narrows not just the static type but
the **value/representation**: the body reads the payload at `T`'s known layout, no re-projection, no
re-seal. This is what deletes the `Conn = Boarding | Transfer` / `Trip | Null` materialization seam
that the performance work kept hitting (`docs/PERFORMANCE.md` §2, the "remaining representation lever").
`T | Null` where `T` is pointer-shaped collapses to a **nullable pointer** (one word, like Go `*T`).

### 5.3 Anonymous structural records

`{ "x": 1, "y": 2 }` with no annotation infers an anonymous structural record type with a known field
set → packed value struct, same as a named record. It only becomes a hashmap when the context type is
`{ String: T }`, and only becomes `Any` when typed `Any`. The disambiguation is by type context and
is essentially already how inference behaves; it just needs to be the *explicit, documented* rule.

### 5.4 Perceus is the engine, not an afterthought

Value semantics is only fast because of **move + reuse**, and Lin already has it (`lin-ir/src/rc_elide.rs`):
- **Last-use → move.** Passing/returning a record that is dead afterward transfers ownership; no copy.
  This is what turns `push(routeArr, trip)` (the RAPTOR PREP regroup) from a `sealed_alloc` copy into
  a move.
- **Reuse-in-place.** A dead buffer of a given shape is reused for the next allocation of that shape
  (the Koka/Lean4 trick that makes functional value data fast).
- Crucially this analysis is **local** (per function, last-use). The "whole-program proof" that the
  current reference default needs is *self-inflicted*; value-by-default removes the need for it. The
  same pass, repointed from "manage shared boxed objects" to "manage owned value records," is what
  makes the model fast.

### 5.5 Records and dynamic key iteration

Decide explicitly: do records support `keys(record)` / dynamic field enumeration? The clean answer
consistent with "types vs hashmaps are different things" is **no** — a record has a fixed, known field
set; if you want to iterate dynamic keys, use a hashmap. This keeps reflection out of the value model.
(`std/object.keys` then applies to hashmaps / `Any`, not records.) This is a small but real
ergonomic/spec decision that should be settled before stage work.

---

## 6. Implementation plan (staged, gated)

The change is pervasive but **stageable by value-shape**, and the gates from the performance work are
strong enough to catch regressions byte-for-byte at each stage. The risk is front-loaded (early stages
prove the deletion model); the payoff is back-loaded (record-arrays and unions are where RAPTOR closes
on Go).

### Per-stage gate (every stage must hold all of these)

- `cargo build --workspace && cargo test --workspace` — 0 failures.
- `lin test stdlib/ examples/` — full green (currently 72/72).
- RAPTOR cross-language **digest byte-identical** (`group=26203913 range=773022892 journeys=139`),
  both `lin/` and `lin-typed/`.
- ASan clean; RSS (`VmHWM`) bounded/flat over the full RAPTOR run (no scaling leak).
- Cross-language bench: `records` still beats Go/Rust; RAPTOR typed does not regress and trends toward
  Go.
- `lin fmt --check` over `stdlib/`/`examples/`/`benchmarks/`.

### Stage 0 — Pin the semantics (no code)

- Spec: add to §5/§6 that **sealed records are value types** (assignment/param/return copy; no
  observable aliasing; sharing is explicit via `Shared<T>`). Make §5.9.1's projection total.
- Spec/ADR: **`Any` is the dynamic top type, a recursive union; there is no boxed string-keyed
  object.** Record the `Json → Any` rename and the "JSON is a wire format only" stance.
- Decide §5.5 (records do not support dynamic key enumeration) and §2.5 (ordered-map strategy).
- New ADR capturing the inversion + this document as its rationale.
- **Deliverable:** signed-off semantics. Everything below keys off this.

### Stage 1 — All-scalar records: make packed value the *unconditional* representation

- These already pack today, so this stage mostly **removes** the boxed-shadow arm in `repr.rs` and the
  "boxed when read from a slot" paths for the all-scalar case, and makes value semantics (move/copy)
  explicit via `rc_elide`.
- Lowest perf risk (the `records` bench is already this path); proves the deletion model and the
  value-semantics test surface (the `var b = a; a["f"]=v` breaking-change inventory starts here).
- **Payoff:** none directly; this is the safe beachhead.

### Stage 2 — Heap-field records: unconditional packed value layout

- Make const-offset reads of `String`/array/map/nested-record fields unconditional (the layout §5.9.1
  already describes). Remove per-access read materialization.
- **Payoff:** kills the query-side *read* seam at the source (no more hand de-materialization).

### Stage 3 — `T[]` as a contiguous buffer of packed `T`

- The "arrays of heap-field records stay boxed" unlock. Element access offset-plus-offset; `push`
  moves/copies inline; in-place index/field assignment.
- **Payoff:** the RAPTOR PREP regroup copy mostly evaporates (with Stage 4); query array reads go
  const-offset. This is the big one.

### Stage 4 — Repoint Perceus/`rc_elide` to value move + reuse

- Last-use → move; reuse dead record/array buffers in place. (Can land incrementally alongside 1–3;
  called out separately because it is what makes value semantics *fast* rather than merely correct.)
- **Payoff:** turns value-copy into value-move on the hot path; PREP construct/regroup approaches Go.

### Stage 5 — Unions of records: tagged value + `match` narrows representation

- `T | Null` → nullable pointer; `A | B` → tag + payload; narrowing reads the payload at the known
  layout (generalize SumNode, ADR-064).
- **Payoff:** deletes the `Conn`/`Trip | Null` materialization seam — the last representation lever.

### Stage 6 — `{ String: T }` maps hold packed values inline

- Mirror Stage 3 for map values.
- **Payoff:** map-of-record reads/writes go const-offset; closes the map-value seam.

### Stage 7 — Dissolve `LinObject`; re-found `Any`

- Object literals default to records; statically-unknown shapes become `{ String: Any }` hashmaps or
  `Any`; `Json → Any` rename throughout (`lin-check`, stdlib, docs).
- Delete `object.rs` / `lin_object_get`.
- Provide an explicit **ordered-map** for the ordering-dependent cases (or migrate them to pair lists),
  per §2.5.
- **Payoff:** the slow string-keyed path ceases to exist; the model is finally coherent. (Parts of
  this — the rename, the ordered-map — can be sequenced earlier; the `object.rs` deletion lands once
  nothing produces a boxed object.)

### Closing work

- Update `docs/SPECIFICATION.md`, `docs/STDLIB.md`, `docs/DECISIONS.md`, `docs/PERFORMANCE.md`.
- Re-measure RAPTOR typed vs Go (the new success metric — see §7).
- Migration notes for the breaking change (§7.2).

---

## 7. Success criteria, risks, and the breaking change

### 7.1 Success criteria

The success metric **changes** under the new model. Today we compare "typed vs `Json` port." After
this work there is no race for the same data: if you type it, it is a record (fast); if you do not,
it is `Any` (slow, *by your choice*). So success is:

- **Typed RAPTOR approaches Go**, not "approaches the Json port." Target: query phases within a small
  constant of Go; PREP within a small constant of Go (the regroup is a move, not a copy).
- **`Any` is the only slow path, and only when explicitly chosen.** The "Json is terrible" property
  becomes a property of *opting into dynamic*, exactly as intended.
- The compiler is **smaller**: `repr.rs` reconciliation, `object.rs`, `BoxKeepPacked`, the path-9
  machinery all deleted.

### 7.2 The breaking change

- Code relying on **record reference aliasing** — `var b = a; a["f"] = v` observed through `b` —
  changes behavior. (`val` cannot mutate, so the surface is narrower than it sounds.) This was never
  an intended semantic; frame it as a correctness fix. Stage 1's value-semantics test surface
  produces the concrete inventory.
- Code relying on **`Json` object insertion-order iteration** must move to an explicit ordered
  container (§2.5).
- The `Json → Any` rename is mechanical but wide (stdlib signatures, examples, docs).

### 7.3 Risks

- **Largest change in the project's history.** It touches `lin-check`, `lin-ir`
  (`repr`/`lower`/`rc_elide`), all of `lin-codegen` (`data`/`boxing`/`types`/`match`/`rc`), and
  `lin-runtime` (`object`/`sealed`/`map`/array). Mitigation: it is *subtractive* and *staged*, with
  byte-level gates at each step. A stage that cannot hold the digest has found code that genuinely
  relied on reference aliasing — i.e. it surfaces the breaking-change inventory rather than hiding it.
- **Large-record copy cost** where move-elision does not fire. Mitigation: the Go/Swift discipline —
  explicit indirection / `Shared<T>` for big shared mutable state — plus small-record inlining as a
  later optimization.
- **`Any` boundary conversion cost** (record ↔ hashmap). Mitigation: keep the typed path wide; plan
  the descriptor-carrying fast path.
- **Scope discipline.** This must not become an open-ended rewrite. Each stage has a defined payoff
  and gate; ship stage-by-stage on `master`, not on a long-lived branch.

---

## 8. The one-sentence version

Stop treating "a record" and "a JSON object" as the same boxed string-keyed thing: make records
value-typed packed structs (the spec already wants this), make dynamic data either a real hashmap or
the `Any` top type, delete `LinObject`, and let the Perceus pass you already have make value
semantics fast — and the typed-vs-Go gap closes as a *consequence* rather than as a perpetual fight.
