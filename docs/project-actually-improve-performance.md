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
*on top of* the JSON runtime object (`LinObject` — a heap-allocated, refcounted, **string-keyed**
thing). Field access on a record therefore became a key lookup and an LLVM optimization barrier, even
when the field set is statically known, and the compiler grew a flow-sensitive representation-inference
pass (ADR-062) to *claw back* struct speed occurrence-by-occurrence — the origin of every "path-9"
dead end. This document proposes the fix along **two clearly separated axes**:

- **Representation (the performance lever — this is what must change):** a record's representation
  becomes a **flat packed struct with constant-offset field access**, never a string-keyed object.
  Dynamic data is either a real **hashmap** (`{ String: T }`) or the **`Any`** top type (née `Json`).
  The boxed string-keyed object (`LinObject`) ceases to exist.
- **Semantics (a deliberate, separate choice — and we are keeping today's behaviour):** records stay
  **reference types**. `val b = a` shares; `mutateObj(b)` mutating its parameter is still visible
  through `b`. This is the Java/C# object model — reference semantics with a *flat* layout and O(1)
  field access — and it was never the source of the slowness.

The crucial realisation is that these two axes are independent. The accident was the *representation*
(string-keyed), not the *reference semantics*. Fixing the representation makes typed code fast;
keeping reference semantics preserves every piece of current userland behaviour. The typed-vs-Go gap
then closes as a *consequence* of drawing three clean lines — types, hashmaps, and a dynamic top
type — rather than as a perpetual fight.

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
  splits into: (a) reading heap-field records out of maps/arrays/unions — *per-access string-keyed
  materialization*; (b) constructing/regrouping record collections — a *copy* cost **that this session
  measured as "inherent," but that result is specific to a value-semantics layout — see §2.4**; (c)
  generic closure call boundaries — *box/unbox*. All three trace back to the representation, not to
  reference semantics. See `docs/PERFORMANCE.md` §2.

### 1.2 The root cause

The runtime grew up around `LinObject`: a boxed, refcounted, **string-keyed** object — the carrier
for JSON. Typed records were implemented *as* `LinObject`s rather than *beside* them as structs. The
load-bearing consequence is one thing, not two:

> **A "record" and a "JSON object" were welded into a single string-keyed representation.** A *record*
> has fixed, statically-known fields and wants a struct (constant-offset access). A *hashmap* has
> dynamic string keys and wants a dictionary. Conflated into one boxed string-keyed object, field
> access is an association-list / hashed lookup and an LLVM optimization barrier — *even when the
> field set is known at compile time*.

Records *also* ended up with reference semantics (`val b = a; a["k"]=v` is visible through `b`),
because two bindings can hold the same `LinObject` pointer. **This part is fine and we are keeping
it** — reference semantics with a flat layout is exactly the Java/C# model and is fast. It was never
the villain; the string-keyed representation was. The mistake to undo is the conflation, not the
sharing.

The representation-inference pass (ADR-062, `lin-ir/src/repr.rs`) exists solely to *recover* struct
speed from the boxed default — deciding, occurrence by occurrence, "packed when freshly constructed,
boxed when read back from a slot." It is the single largest source of implementation complexity and
the origin of every "path-9" dead end. It is a workaround for the wrong default. With one flat
representation per record, it collapses to a layout calculator.

*(Note on §5.9.1: the spec's "non-mutating projection" copies a value when it is narrowed to a record
type `T`, dropping extra fields. That is a **field-dropping narrowing** operation and is compatible
with either value or reference assignment semantics — it does **not** mandate value semantics, and the
earlier reading of it as "the spec leans value" was an over-read. Under reference semantics, narrowing
to `T` still produces a fresh flat struct with exactly `T`'s fields; same-type assignment shares.)*

---

## 2. The target model

There are exactly these kinds of values. Nothing else. In particular there is **no boxed
string-keyed object**.

| Kind | Type form | Representation | Semantics | Field/elem access |
|------|-----------|----------------|-----------|-------------------|
| Scalar | `Int32`/`Float64`/`Bool`/… | inline machine value | value | n/a |
| String | `String` | refcounted byte buffer (handle) | reference | n/a |
| Array | `T[]` | pointer-backed buffer of `T` | reference | const-offset / deref |
| Hashmap | `{ String: T }` | hashed `LinMap` | reference | O(1) hash lookup |
| Record | `type P = {…}` / anon structural | **flat packed struct** (heap, pointer-shared) | **reference** | **constant-offset load** |
| Union | `A \| B`, `T \| Null` | tag word + payload (ptr, or nullable ptr) | follows member | `match … is T` → read payload at `T`'s layout |
| Any | `Any` (née `Json`) | recursive tagged union; see §2.5 | reference | dispatch on tag |
| Opaque handle | `Function`, `Iterator`, `Promise`, `Stream`, `Shared`, `Frozen`, `TarEntry` | nominal runtime handle | reference | n/a |

The three lines the current design blurs, drawn sharply:

- **Types** (records) — fixed fields, **reference** semantics, **flat struct** layout, constant-offset access.
- **Hashmaps** (`{ String: T }`) — dynamic string keys, a real dictionary, O(1) lookup.
- **A dynamic top type** (`Any`) — "I don't know the shape; dispatch at runtime and pay for it."

### 2.1 The two axes, decided

- **Representation (required): records are flat packed structs.** A record is a pointer to a
  contiguous heap struct: scalar fields inline at their natural offset, heap fields (`String`, array,
  map, nested record) as 8-byte owned pointer slots. Field access is **always** a constant-offset
  load — never a key lookup — whether the record was just constructed or read out of an array, map, or
  union. There is no boxed shadow and no "read from a slot makes it string-keyed" arm. This is the
  whole performance change.
- **Semantics (chosen): records are reference types.** Assignment and parameter passing **share** the
  pointer; there is no copy and no behaviour change from today. `val b = a` makes `b` and `a` the same
  record; `mutateObj(b)` mutating its parameter is visible through `b`. Mutation writes through the
  shared pointer, visible to all aliases — exactly the current observable behaviour. *(A `val` binding
  is still immutable as a binding; reference semantics is about what assignment/passing does with the
  record, which is share.)*

This is the Java/C# object model: reference types, flat layout, O(1) fields. The only userland-visible
changes in the whole project are the `Json → Any` rename (§2.5) and the loss of `Json`-object
insertion-order iteration (§2.6). **Passing records to functions does not change. Aliasing does not
change. In-place mutation through a parameter does not change.**

### 2.2 Why reference semantics here is fast *and* simpler

- **Constant-offset field access** through the pointer is the entire query-side win — the string-keyed
  scan is gone. One extra pointer dereference vs. an inline value layout, but every step is O(1).
- **No correctness obligation on aliasing analysis.** A value-semantics model needs move/last-use
  analysis to avoid copying on every assignment; reference semantics shares by default, so the
  analysis (Perceus, §5.4) becomes a pure *optimization*, never a correctness requirement.
- **Share-into-collections is cheap.** Because `T[]`/`{String:T}` are pointer-backed (§2.3),
  `push(routeArr, trip)` shares a pointer — like the `Json` form, no copy. This is what dissolves the
  RAPTOR PREP "inherent regroup copy" (§2.4): it was inherent only to a value/inline layout.

### 2.3 Arrays and maps are pointer-backed

- `T[]` is a buffer of pointers to flat record structs (like Java `T[]`), **not** a buffer of
  string-keyed objects. `arr[i]` is a deref to the shared record; `arr[i]["f"]` is deref +
  constant-offset; `arr[i]["f"] = v` writes the shared record in place (visible to aliases);
  `push(arr, r)` appends a shared pointer (cheap).
- `{ String: T }` stores record pointers as values; reads return the shared record.
- The key change from today is **what the pointer points at**: a flat struct (constant-offset) instead
  of a string-keyed object. The pointer-backed array structure itself is largely as it is now, so the
  "arrays of heap-field records stay boxed" limitation (§5.9.1) stops mattering — the elements are fast
  because they are flat, with no need to make the array inline-contiguous.
- **Optional later optimization:** for scalar-dense, non-escaping arrays the compiler may inline
  elements contiguously (Go `[]T`, better cache locality). This needs escape/uniqueness analysis and
  is *not* required for the core win; it is the one place a value-style layout buys something, and it
  is deferred (§6, optional stage). Scalar arrays are already inline (`lin_flat_array_*`).

### 2.4 What this does to the "inherent PREP copy" finding

`docs/PERFORMANCE.md` §2 records PREP's ~3.67× as an *inherent* cost: regrouping trips into
`tripsByRoute` copies each `Trip`. **That is inherent only under value-semantics / inline-array
layout.** Under reference semantics with pointer-backed arrays, the regroup shares a pointer — cheap,
like `Json` — so the "inherent" caveat **does not apply** to this model. PREP becomes fast for the
same reason the query path does: flat records + pointer sharing. (Once this model lands, that section
of `PERFORMANCE.md` should be updated.)

### 2.5 `Any` is a type, not a representation (the `Json` dissolution)

`Json` is renamed and re-founded as **`Any`** — the dynamic top type. It is a recursive union:

```
Any  =  Null | Bool | Int* | Float* | String | Any[] | { String: Any } | <opaque handle>
```

- Its representation is the **tagged-union machinery you already have** (the tagged value / SumNode
  family, ADR-064). It is **not** a bespoke object.
- A *"JSON object"* is therefore just a `{ String: Any }` **hashmap**. There is no third thing.
- Index/field access on an `Any` dispatches on the tag: hashmap → hash lookup; array → index;
  otherwise the safe-access rule yields `Null`. Appropriately dynamic, and honestly slow — which is
  what `Any` is *for*.
- The boundary, and the only place dynamic↔typed conversion happens:
  - **`T` → `Any`**: box. Start simple — project the record to a `{ String: Any }` hashmap (you have
    lost the static type at the dynamic boundary anyway). Later optimization: carry a record pointer +
    descriptor and fast-path the round trip.
  - **`Any` → `T`**: a validating projection / `fromJson`-style construction of a flat `T` struct
    (already specified by §5.9.1's projection).
- Rename rationale: `Any` reads as exactly what it is and stops implying a special JSON runtime
  object. JSON becomes purely a **wire format** — parsed into records/hashmaps/`Any`, serialized out
  of them — never a thing resident in memory.

### 2.6 The wrinkle: iteration order

The current "Json object" quietly provides **insertion-ordered** key iteration, and a little code
leans on it. Concretely: RAPTOR's `getQueue` keeps `Json` *specifically* because the within-round
tie-break between equal-arrival trips depends on key insertion order; a hash-ordered map changed which
trip a journey boarded and broke the cross-language digest (documented in
`benchmarks/compare/raptor/lin-typed/queueFactory.lin`).

A `{ String: T }` hashmap iterates in hash order. So when the Json object is dissolved, the ordering
guarantee must move somewhere **explicit**: either an *ordered-map* container variant (a linked
hashmap that preserves insertion order) for the few places that need it, or those places switch to a
list of `(key, value)` pairs. The right outcome is that ordering becomes an explicit property of an
explicit container, not a hidden affordance — but it is a conscious migration, not a free swap. This
is one of only two userland-visible changes (the other being the `Json → Any` rename).

---

## 3. What gets deleted

This is a *simplifying* rewrite. The net line count goes **down**, and the mental model gets smaller.

- `lin-runtime/src/object.rs` and `lin_object_get`'s string-keyed scan — **gone**. No value exists on
  which you do a string-keyed lookup over an unknown field set; dynamic access is a hashmap O(1)
  lookup or an `Any` tag dispatch.
- The reconciliation arms of `lin-ir/src/repr.rs` (the flow-sensitive "packed-or-boxed" oracle/verify
  logic, the ADR-062 §H4/H5 machinery). What remains is a *layout calculator* (compute offsets;
  choose inline scalar vs owned-pointer field slots). Because every record has **one** representation
  (a flat struct), there is no packed-vs-boxed to reconcile.
- `BoxKeepPacked` and the keep-packed-across-boundary machinery — there is no second representation to
  keep packed against.
- The boxed-shadow paths in `lin-codegen/src/codegen/boxing.rs`, the per-access materialize in
  `data.rs`, and `sealed.rs`'s rebuild-from-boxed.
- Essentially the entire "path-9" problem space, because it was the symptom of having two
  representations for one thing.

---

## 4. What we are explicitly NOT changing (non-goals)

- **Userland record semantics are unchanged.** Reference semantics, parameter passing, aliasing,
  in-place mutation through a parameter — all identical to today. (Only `Json → Any` and the
  ordered-iteration wrinkle are visible.)
- **Memory management stays Perceus-style RC.** Measured not alloc-bound; a tracing GC buys ~0%.
- **Concurrency stays share-nothing** (deep-copy on transfer, `Shared`/`Frozen`, worker-owned state).
- **Eager combinator fusion stays.** Already beats Rust; untouched.
- **The surface syntax is unchanged.** Braces still mean a record when the field set is statically
  known and a hashmap when the context type is `{ String: T }`.
- **No new general-purpose value/move feature is required.** Reference-by-default needs none. (If a
  value-type record mode is ever wanted as an opt-in for cache-dense data, it is a *future* addition,
  not part of this work.)

---

## 5. Design details that need to be right

### 5.1 Record ↔ `Any` boundary cost

Boxing a record to `Any` by converting to a `{ String: Any }` hashmap is a real cost paid at the
moment you go dynamic. Acceptable (it is the escape hatch), but: keep the typed path wide so values
rarely *need* to become `Any`, and plan the later optimization where `Any` carries a record pointer +
descriptor and the `Any → T` projection fast-paths when the descriptor already matches `T`.

### 5.2 Unions and `match` narrowing the *value*

With a single flat representation per record, `match x is T => …` narrows not just the static type but
the **value/representation**: the body reads the payload at `T`'s known layout, no re-projection, no
re-seal. This deletes the `Conn = Boarding | Transfer` / `Trip | Null` materialization seam the
performance work kept hitting (`docs/PERFORMANCE.md` §2). `T | Null` where `T` is a record collapses to
a **nullable pointer** (one word, like Go `*T`).

### 5.3 Anonymous structural records

`{ "x": 1, "y": 2 }` with no annotation infers an anonymous structural record type with a known field
set → flat struct, same as a named record. It becomes a hashmap only when the context type is
`{ String: T }`, and `Any` only when typed `Any`. The disambiguation is by type context and is
essentially already how inference behaves; it just needs to be the *explicit, documented* rule.

### 5.4 Perceus is an optimization here, not a correctness requirement

Reference semantics shares by default, so nothing about correctness depends on move analysis. But
`lin-ir/src/rc_elide.rs` still pays for itself as an optimization:
- **Reuse-in-place.** A record whose last reference is dropped can have its buffer reused for the next
  allocation of the same shape, cutting allocator traffic.
- **RC elision.** Dead retains/releases around borrows are removed (already what it does).
This is upside, sequenced after the core win, and never a blocker — a contrast with the value-semantics
plan, where move analysis would have been load-bearing for *avoiding copies*.

### 5.5 Records and dynamic key iteration

Decide explicitly: do records support `keys(record)` / dynamic field enumeration? The clean answer
consistent with "types vs hashmaps are different things" is **no** — a record has a fixed, known field
set; if you want to iterate dynamic keys, use a hashmap. `std/object.keys` then applies to hashmaps /
`Any`, not records. Small but real spec decision; settle it before stage work.

### 5.6 The optional inline-array (value-layout) optimization

The one thing a value/inline layout buys over pointer-backed arrays is cache locality on dense
iteration (Go `[]T` vs Java `T[]`). Under reference semantics this is a *local optimization*: where a
`T[]` provably does not alias-escape its elements, lay them out inline. It requires escape/uniqueness
analysis, is not needed for the core win, and is deferred. Calling it out so it is on the roadmap as
the route to *closing the last constant* on Go for scan-dense code.

---

## 6. Implementation plan (staged, gated)

The change is pervasive but **stageable by value-shape**, and — because semantics are unchanged —
each stage is a pure *representation* swap that the existing gates catch byte-for-byte. Reference
semantics makes the hardest part of the original plan (inline-contiguous record arrays) **optional**:
the win flows through pointer-backed arrays automatically once records are flat (Stage 2).

### Per-stage gate (every stage must hold all of these)

- `cargo build --workspace && cargo test --workspace` — 0 failures.
- `lin test stdlib/ examples/` — full green (currently 72/72).
- RAPTOR cross-language **digest byte-identical** (`group=26203913 range=773022892 journeys=139`),
  both `lin/` and `lin-typed/`.
- ASan clean; RSS (`VmHWM`) bounded/flat over the full RAPTOR run (no scaling leak).
- Cross-language bench: `records` still beats Go/Rust; RAPTOR typed does not regress and trends toward
  Go.
- `lin fmt --check` over `stdlib/`/`examples/`/`benchmarks/`.

Because record semantics do not change, **the RAPTOR digest is expected to stay byte-identical through
every stage** — there is no behaviour migration interleaved with the representation work (that is the
big advantage of choosing reference over value). The only deliberately behaviour-affecting work is
Stage 6 (the `Json`/ordered-iteration migration), which is isolated at the end.

### Stage 0 — Pin the decisions (no code)

- Spec/ADR: record the **two axes** — representation becomes flat packed struct (required); semantics
  stay **reference** (chosen, no userland change). State that `LinObject` is removed and there is one
  representation per record.
- Spec/ADR: **`Any` is the dynamic top type, a recursive union; there is no boxed string-keyed
  object.** Record the `Json → Any` rename and "JSON is a wire format only."
- Decide §5.5 (records do not support dynamic key enumeration) and §2.6 (ordered-map strategy).
- New ADR capturing the inversion + this document as its rationale; supersede/annotate ADR-062.
- **Deliverable:** signed-off decisions. Everything below keys off them.

### Stage 1 — All-scalar records: one flat representation, unconditional

- These already pack today, so this stage mostly **removes** the boxed-shadow arm in `repr.rs` and the
  "boxed when read from a slot" paths for the all-scalar case, making the flat struct the sole
  representation. Reference semantics is unchanged, so this is a pure internal simplification.
- Lowest risk (the `records` bench is already this path); proves the deletion model.
- **Payoff:** none directly; the safe beachhead, and it deletes real reconciliation code.

### Stage 2 — Heap-field records: flat struct with constant-offset fields, unconditional

- Make const-offset reads of `String`/array/map/nested-record fields the sole representation (the
  layout §5.9.1 already describes). Remove the string-keyed materialization on read.
- Because arrays and maps are pointer-backed, **this automatically makes `Trip[]` / `{String:Trip}`
  fast** — the elements are now flat structs behind the existing pointers. The "arrays of heap-field
  records stay boxed" problem dissolves without an inline-array rewrite.
- **Payoff:** the big one — kills the query-side read seam *and* (with pointer-backed collections) the
  PREP regroup cost, at the source.

### Stage 3 — Unions of records: tagged value + `match` narrows representation

- `T | Null` → nullable pointer; `A | B` → tag + payload-pointer; narrowing reads the payload at the
  known layout (generalize SumNode, ADR-064).
- **Payoff:** deletes the `Conn` / `Trip | Null` materialization seam — the last representation lever
  from the perf work.

### Stage 4 — Repoint Perceus/`rc_elide` as a record optimization

- Reuse-in-place for dead record buffers; tidy RC around borrows. Pure upside; never a blocker
  (semantics already correct without it).
- **Payoff:** cuts allocator traffic; tightens the constant vs Go.

### Stage 5 — Hashmap/array value representation polish

- Ensure `{ String: T }` and `T[]` store record pointers uniformly and reads return the shared record
  with no materialization. (Largely falls out of Stage 2; this is the cleanup/verify pass.)
- **Payoff:** confirms no residual materialization at map/array boundaries.

### Stage 6 — Dissolve `LinObject`; re-found `Any` (the only userland migration)

- Object literals default to records; statically-unknown shapes become `{ String: Any }` hashmaps or
  `Any`; `Json → Any` rename throughout (`lin-check`, stdlib, docs, examples).
- Delete `object.rs` / `lin_object_get`.
- Provide an explicit **ordered-map** for the ordering-dependent cases (or migrate them to pair lists),
  per §2.6.
- **Payoff:** the slow string-keyed path ceases to exist; the model is finally coherent. This is the
  one stage with deliberate userland-visible change; isolating it last keeps Stages 1–5 digest-stable.

### Optional later — inline-array (value-layout) optimization (§5.6)

- Escape/uniqueness analysis to lay non-escaping `T[]` elements out contiguously for cache locality.
- **Payoff:** closes the last constant on Go for scan-dense code. Not required for the headline result.

### Closing work

- Update `docs/SPECIFICATION.md`, `docs/STDLIB.md`, `docs/DECISIONS.md`, `docs/PERFORMANCE.md`
  (including removing the "inherent PREP copy" caveat — §2.4).
- Re-measure RAPTOR typed vs Go (the new success metric — see §7).

---

## 7. Success criteria, risks, and the (small) breaking change

### 7.1 Success criteria

The success metric **changes** under the new model. Today we compare "typed vs `Json` port." After
this work there is no race for the same data: if you type it, it is a record (fast); if you do not, it
is `Any` (slow, *by your choice*). So success is:

- **Typed RAPTOR approaches Go**, not "approaches the Json port." Expect a small residual constant from
  per-record pointer indirection + per-record allocation (the price of reference + pointer-backed
  arrays); the optional inline-array optimization (§5.6) closes that for scan-dense code.
- **`Any` is the only slow path, and only when explicitly chosen.** "Json is terrible" becomes a
  property of *opting into dynamic*, exactly as intended.
- The compiler is **smaller**: `repr.rs` reconciliation, `object.rs`, `BoxKeepPacked`, the path-9
  machinery all deleted.

### 7.2 The breaking change (now small)

Because semantics are unchanged, the userland-visible surface is just:

- **`Json → Any` rename** — mechanical but wide (stdlib signatures, examples, docs).
- **`Json`-object insertion-order iteration** moves to an explicit ordered container (§2.6).

That is all. **`val b = a` aliasing, passing records to functions, and in-place mutation through a
parameter (`mutateObj(b)`) are unchanged** — they keep today's reference behaviour. This is the
central reason for choosing reference semantics: the performance win with essentially no behaviour
migration.

### 7.3 Risks

- **Large change, but subtractive and digest-stable.** It touches `lin-check`, `lin-ir`
  (`repr`/`lower`/`rc_elide`), all of `lin-codegen` (`data`/`boxing`/`types`/`match`/`rc`), and
  `lin-runtime` (`object`/`sealed`/`map`/array) — but Stages 1–5 do not change behaviour, so the
  RAPTOR digest and the full suite are expected to stay byte-identical throughout, which is a strong,
  cheap guard. A stage that *cannot* hold the digest has found a real representation bug, not a
  semantics migration.
- **Pointer indirection / allocation residual** vs Go's inline arrays. Mitigation: the optional
  inline-array optimization (§5.6); and per-record allocation is already measured not-bottleneck.
- **`Any` boundary conversion cost** (record ↔ hashmap). Mitigation: keep the typed path wide; plan
  the descriptor-carrying fast path.
- **Scope discipline.** Each stage has a defined payoff and gate; ship stage-by-stage on `master`, not
  on a long-lived branch.

---

## 8. The one-sentence version

Stop representing "a record" as a string-keyed JSON object: make a record a pointer to a **flat packed
struct** with constant-offset access (keeping today's **reference** semantics, so no userland behaviour
changes), make dynamic data either a real hashmap or the `Any` top type, delete `LinObject`, and the
typed-vs-Go gap closes as a *consequence* — with the only userland changes being `Json → Any` and a
small ordered-iteration migration.
