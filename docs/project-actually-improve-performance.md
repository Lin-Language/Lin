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
dead end. The fix is along **two clearly separated axes**: a record's *representation* becomes a flat
packed struct with constant-offset access (the performance lever), while its *semantics* stay
**reference** — `val b = a` shares, `mutateObj(b)` mutating its parameter is still visible (no userland
behaviour change from the axis we keep). And the conflated JSON object is dissolved: dynamic data is
either a real **hashmap** (`{ String: T }`) or **`AnyVal`** (née `Json`), and `LinObject` ceases to
exist. The typed-vs-Go gap then closes as a *consequence* of drawing three clean lines — types,
hashmaps, and a dynamic value type — rather than as a perpetual fight.

---

## 0.5 Stage-0 decisions (PINNED — everything keys off these)

These resolve the design holes found in review. They are decisions, not open questions; the staged
plan (§6) assumes them.

- **D1 — Representation vs semantics are separate axes.** Representation: records become flat packed
  structs (constant-offset access), required. Semantics: records stay **reference** types (Java/C#
  model — flat layout, O(1) fields, pointer-shared). Passing records to functions, `val b = a`
  aliasing, and in-place mutation through a parameter are **unchanged**.

- **D2 — `Json` → `AnyVal`, a single JSON-shaped value union, with NO opaque handles.**
  `AnyVal = Null | Bool | Int* | Float* | String | AnyVal[] | { String: AnyVal } | <any record>`.
  It is **value-shaped only**: it cannot hold a `Function`, `Iterator`, `Stream`, `Shared`, `Promise`,
  or `TarEntry`. There is **no** separate handle-carrying top type above it — handles stay statically
  typed and cannot be widened into `AnyVal`. This is deliberate: it preserves the gates that depend on
  the dynamic type being JSON-shaped (cross-thread transferability, the async-thunk return
  restriction, foreign-signature exclusions). The project goal is to **retire almost all uses of
  `AnyVal`** — most values currently typed `Json` get a precise type (a record, a hashmap, a union);
  `AnyVal` survives only as the genuine "unknown wire shape" escape hatch. (Naming note: it is called
  `AnyVal`, not `Any`, precisely because it is *not* a true top type — `print`/display/serialization
  accept `AnyVal`, the set of *displayable value shapes*, not anything-including-handles.)

- **D3 — Anonymous structural parameter types monomorphise per concrete argument layout.** A function
  over `(r: { "type": String })` is specialised per caller's concrete record layout (offset of `type`
  may differ), exactly as generics already monomorphise. This preserves sharing and adds no new
  concept. The residual — a **stored closure** `(Named) => _` invoked with heterogeneous layouts — uses
  **project-copy at the closure boundary** as the uniform fallback (the closure already crosses an
  opaque boundary, so a copy there is acceptable). This extends §5.3 from literals to parameter
  boundaries.

- **D4 — The record↔`AnyVal` boundary is a defined single-direction conversion, not a reconciliation
  oracle.** `record → AnyVal` **carries the record pointer + its descriptor** (preserving reference
  semantics and aliasing through the dynamic boundary; **no** deep O(graph) hashmap conversion). This
  is the **v1** design, not a later optimization — it is the compatibility-preserving option, because
  converting to a `{String:AnyVal}` hashmap would sever the aliasing that widening-to-`Json` has today.
  `AnyVal → record` validates-and-projects (the §5.9.1 projection). There is no bidirectional
  packed-or-boxed oracle — that was the path-9 trap. (`PERFORMANCE.md`'s path-9 epitaph "each boundary
  is a materialize-or-leak seam" applied to a *reconciliation*; this is a one-way conversion.)

- **D5 — Aliasing is unified to share-always; this is a real, intended, observable change.** Today a
  packed-value array `push` **copies** the element (the measured PREP 3.67×; §2.4) while a boxed array
  `push` **shares** a pointer — so `push(arr, t); t["x"] = 5; arr[i]["x"]` already differs by
  representation. Unifying to one representation makes it consistently observe `5`. This is the better
  semantics (one representation, one behaviour), but it is a behaviour change and means **digest
  stability across stages is an *expectation*, not a guarantee** — a stage that breaks the RAPTOR
  digest may have found *this intended change*, not a bug. A directed test pins the intended behaviour.

- **D6 — `keys`/`values`/`entries` apply to hashmaps and `AnyVal`, not records.** A record has a
  fixed, statically-known field set; dynamic key enumeration uses a hashmap. `std/object` currently
  applies to any object; narrowing it to hashmaps/`AnyVal` is a visible change.

- **D7 — Descriptors are a KEPT runtime concept.** Deleting `LinObject` deletes string-keyed
  *storage*, but record field-name/offset **descriptors** remain (they already half-exist for sealed
  records) and drive: order-independent equality, `toString`/display, `is T`/`has T` after a value has
  been through `AnyVal`, `fromJson` validation, worker deep-copy, and JSON serialization. The "net line
  count goes down" claim (§3) is judged against this residual.

- **D8 — The boxed shadow survives for `AnyVal`-flowing records until Stage 6 (transitional).** During
  Stages 1–5, `LinObject` still exists, so a record widened into an `AnyVal` slot keeps a boxed/
  descriptor form until the `AnyVal` refounding (Stage 6). Stage 1's "removes the boxed-shadow arm" and
  §3's deletion list are therefore fully realised only at Stage 6; before then they apply to
  non-`AnyVal`-flowing records.

The honest count of userland-visible changes is therefore **four** (§7.2): the `Json → AnyVal` rename,
ordered-iteration migration, `keys`/`values`/`entries` off records (D6), and the aliasing unification
(D5).

---

## 1. Why we are here (the diagnosis)

### 1.1 What the measurements actually say

- **The typed scalar core is already at or above systems-language parity.** The `records`
  cross-language benchmark (sealed all-scalar structs, constant-offset field access) is **Lin 200 ms
  vs Rust 224 ms vs Go 624 ms** — Lin wins. The compilation model is not the problem.
- **Eager combinator chains beat Rust ~4×** (fused to a single zero-allocation loop). Not the problem.
- **A tracing GC would not help.** Measured (`LIN_NO_RC` ceiling): deleting *all* allocation + RC
  recovers ~0% on every workload. No workload is allocation-bound. RC stays.
- **The gap is concentrated at the typed-record representation boundary.** Fully-typed RAPTOR runs
  **1.96×** the `Json` port. The residual is per-access string-keyed materialization (reads), a copy
  cost on construct/regroup (value-layout-specific; §2.4), and generic closure call boundaries — all
  tracing back to the representation, not to reference semantics. See `docs/PERFORMANCE.md` §2.
- **Not every workload is repr-bound — interp is not.** A direct op-cycle profile of the `interp`
  benchmark puts boxed-record reads at ~6%, box/unbox at ~4%, strings at ~0.5%; the bulk is the
  generated code's call/control-flow overhead. The representation reset helps interp only marginally —
  interp needs a separate call-cost/inlining project. This is scoped *out* of this document (it is the
  residual "call axis"), but recorded so the reset is not oversold as a universal fix.

### 1.2 The root cause

The runtime grew up around `LinObject`: a boxed, refcounted, **string-keyed** object — the carrier
for JSON. Typed records were implemented *as* `LinObject`s rather than *beside* them as structs. The
load-bearing consequence is one thing, not two:

> **A "record" and a "JSON object" were welded into a single string-keyed representation.** A *record*
> has fixed, statically-known fields and wants a struct (constant-offset access). A *hashmap* has
> dynamic string keys and wants a dictionary. Conflated into one boxed string-keyed object, field
> access is an association-list / hashed lookup and an LLVM optimization barrier — *even when the
> field set is known at compile time*.

Records *also* ended up with reference semantics because two bindings can hold the same `LinObject`
pointer. **This part is fine and we are keeping it** — reference semantics with a flat layout is the
Java/C# model and is fast. It was never the villain; the string-keyed representation was. The mistake
to undo is the conflation, not the sharing.

The representation-inference pass (ADR-062, `lin-ir/src/repr.rs`) exists solely to *recover* struct
speed from the boxed default. It is the single largest source of implementation complexity and the
origin of every "path-9" dead end. With one flat representation per record, it collapses to a layout
calculator.

*(Note on §5.9.1: the spec's "non-mutating projection" copies a value when it is narrowed to a record
type `T`, dropping extra fields. That is a field-dropping narrowing operation, compatible with either
value or reference assignment semantics — it does not mandate value semantics.)*

---

## 2. The target model

There are exactly these kinds of values. Nothing else. In particular there is **no boxed
string-keyed object**, and **no value type above `AnyVal`**.

| Kind | Type form | Representation | Semantics | Field/elem access |
|------|-----------|----------------|-----------|-------------------|
| Scalar | `Int32`/`Float64`/`Bool`/… | inline machine value | value | n/a |
| String | `String` | refcounted byte buffer (handle) | reference | n/a |
| Array | `T[]` | pointer-backed buffer of `T` | reference | const-offset / deref |
| Hashmap | `{ String: T }` | hashed `LinMap` | reference | O(1) hash lookup |
| Record | `type P = {…}` / anon structural | **flat packed struct** (heap, pointer-shared) | **reference** | **constant-offset load** |
| Union | `A \| B`, `T \| Null` | tag word + payload (ptr, or nullable ptr) | follows member | `match … is T` → read payload at `T`'s layout |
| AnyVal | `AnyVal` (née `Json`) | tagged union over the value kinds + record-ptr+descriptor; see §2.5 | reference | dispatch on tag |
| Opaque handle | `Function`, `Iterator`, `Promise`, `Stream`, `Shared`, `Frozen`, `TarEntry` | nominal runtime handle | reference | n/a — **not** an `AnyVal` member |

The three lines the current design blurs, drawn sharply:

- **Types** (records) — fixed fields, **reference** semantics, **flat struct** layout, constant-offset access.
- **Hashmaps** (`{ String: T }`) — dynamic string keys, a real dictionary, O(1) lookup.
- **A dynamic value type** (`AnyVal`) — "I don't know the shape; dispatch at runtime and pay for it" —
  but JSON-shaped, so it cannot smuggle a handle.

### 2.1 The two axes, decided (D1)

- **Representation (required): records are flat packed structs.** A record is a pointer to a
  contiguous heap struct: scalar fields inline, heap fields (`String`, array, map, nested record) as
  8-byte owned pointer slots. Field access is **always** a constant-offset load. There is no boxed
  shadow (modulo the D8 transitional `AnyVal` path). This is the whole performance change.
- **Semantics (chosen): records are reference types.** Assignment and parameter passing **share** the
  pointer. `val b = a` makes `b` and `a` the same record; `mutateObj(b)` mutating its parameter is
  visible through `b`. Mutation writes through the shared pointer, visible to all aliases.

### 2.2 Why reference semantics here is fast *and* simpler

- **Constant-offset field access** through the pointer is the entire query-side win.
- **No correctness obligation on aliasing analysis.** Reference shares by default, so Perceus (§5.4)
  becomes a pure *optimization*, never a correctness requirement.
- **Share-into-collections is cheap.** Pointer-backed `T[]`/`{String:T}` make `push(routeArr, trip)` a
  pointer share — like the `Json` form, no copy. This dissolves the PREP "inherent regroup copy"
  (§2.4).

### 2.3 Arrays and maps are pointer-backed

- `T[]` is a buffer of pointers to flat record structs (like Java `T[]`). `arr[i]["f"]` is deref +
  constant-offset; `arr[i]["f"] = v` writes the shared record in place; `push(arr, r)` appends a shared
  pointer.
- `{ String: T }` stores record pointers as values; reads return the shared record.
- The key change from today is **what the pointer points at**: a flat struct instead of a string-keyed
  object — so the "arrays of heap-field records stay boxed" limitation stops mattering without an
  inline-array rewrite.
- **Optional later optimization:** inline contiguous element layout for non-escaping `T[]` (Go `[]T`)
  via escape analysis — better cache locality, not required for the core win (§5.6).

### 2.4 What this does to the "inherent PREP copy" finding (and D5)

`docs/PERFORMANCE.md` §2 records PREP's ~3.67× as an *inherent* copy cost. **That is inherent only
under value-semantics / inline-array layout.** Under reference + pointer-backed arrays, the regroup
shares a pointer — cheap. The flip side is **D5**: today, the *currently-packed* cases copy on `push`,
so unifying to share-always is an observable aliasing change (`push(arr,t); t["x"]=5; arr[i]["x"]`
flips to `5`). This is intended and better, but it is named as a behaviour change with a directed test,
and it is why digest stability across stages is an *expectation*, not a guarantee (§6 preamble).

### 2.5 `AnyVal` is a JSON-shaped dynamic value type (the `Json` dissolution) — D2, D4

`Json` is renamed and re-founded as **`AnyVal`** — a recursive **value** union:

```
AnyVal  =  Null | Bool | Int* | Float* | String | AnyVal[] | { String: AnyVal } | <any record>
```

- Its representation is the **tagged-union machinery you already have** (the tagged value / SumNode
  family, ADR-064), with the record case carried as **record pointer + descriptor** (D4). It is **not**
  a bespoke object, and it has **no opaque-handle case** (D2): an `AnyVal` can never hold a
  `Function`/`Iterator`/`Stream`/`Shared`/`Promise`/`TarEntry`.
- A *"JSON object"* with statically-unknown keys is a `{ String: AnyVal }` **hashmap**. There is no
  third thing.
- Index/field access on an `AnyVal` dispatches on the tag: hashmap → hash lookup; record → descriptor
  offset; array → index; otherwise the safe-access rule yields `Null`.
- The boundary (D4), the only place dynamic↔typed conversion happens:
  - **`T` → `AnyVal`**: carry the record pointer + descriptor. **Shares** the record (preserves
    reference semantics and aliasing through the boundary); no deep hashmap conversion. v1 design.
  - **`AnyVal` → `T`**: validating projection / `fromJson`-style construction (§5.9.1).
- `print`, display, and serialization accept **`AnyVal`** — the set of displayable value shapes — not a
  handle-carrying top type. JSON is purely a **wire format**: parsed into records/hashmaps/`AnyVal`,
  serialized out of them, never resident in memory.

### 2.6 The wrinkle: iteration order

The current "Json object" provides **insertion-ordered** key iteration, and a little code leans on it
(RAPTOR's `getQueue` keeps `Json` for the within-round tie-break; hash order broke the digest). When
the Json object is dissolved, that guarantee moves to an **explicit** ordered-map container (a linked
hashmap) for the few cases that need it, or those cases switch to a list of `(key, value)` pairs. One
of the four userland-visible changes.

---

## 3. What gets deleted (and what is kept — D7)

A *simplifying* rewrite. Net line count goes **down**, judged against the kept descriptor residual.

- `lin-runtime/src/object.rs` and `lin_object_get`'s string-keyed **storage + scan** — gone. Dynamic
  access is a hashmap O(1) lookup or an `AnyVal` tag dispatch (record case → descriptor offset).
- The reconciliation arms of `lin-ir/src/repr.rs` (the flow-sensitive packed-or-boxed oracle/verify).
  What remains is a *layout calculator*. (Fully realised at Stage 6 per D8.)
- `BoxKeepPacked` and the keep-packed-across-boundary machinery.
- The boxed-shadow paths in `lin-codegen/src/codegen/boxing.rs`, the per-access materialize in
  `data.rs`, and `sealed.rs`'s rebuild-from-boxed.
- Essentially the entire "path-9" problem space.

**KEPT (D7): record descriptors** — field-name/offset tables (already half-present for sealed records).
They drive equality (order-independent), `toString`/display, `is T`/`has T` after `AnyVal`, `fromJson`
validation, worker deep-copy, and JSON serialization. "Delete `object.rs`" means "replace string-keyed
storage with descriptor-driven walks," not "remove all runtime knowledge of field names."

---

## 4. What we are explicitly NOT changing (non-goals)

- **Userland record semantics on the reference axis are unchanged.** Passing, aliasing, in-place
  mutation through a parameter — identical to today. (The four visible changes are listed in §7.2.)
- **Memory management stays Perceus-style RC.** Measured not alloc-bound.
- **Concurrency stays share-nothing.** `AnyVal`'s no-handle rule (D2) keeps transferability intact.
- **Eager combinator fusion stays.** Already beats Rust; untouched.
- **The surface syntax is unchanged.** Braces mean a record when fields are known, a hashmap under a
  `{ String: T }` context.
- **interp's call-cost axis is out of scope** (§1.1) — a separate project.

---

## 5. Design details that need to be right

### 5.1 Record ↔ `AnyVal` boundary (D4)

`T → AnyVal` carries pointer + descriptor (shares, O(1)); `AnyVal → T` validates-and-projects. No deep
conversion, no oracle. Keep the typed path wide so values rarely become `AnyVal` at all.

### 5.2 Unions and `match` narrowing the *value*

With a single flat representation per record, `match x is T => …` narrows the **value/representation**:
the body reads the payload at `T`'s known layout, no re-projection. This deletes the
`Conn = Boarding | Transfer` / `Trip | Null` materialization seam. `T | Null` where `T` is a record
collapses to a **nullable pointer**.

### 5.3 Structural types — literals *and* parameter boundaries (D3)

- A literal `{ "x": 1, "y": 2 }` infers an anonymous structural record → flat struct (same as a named
  record), a hashmap under `{ String: T }` context, `AnyVal` under `AnyVal`.
- A **parameter** of anonymous structural type (`(r: { "type": String })`) is **monomorphised per
  concrete argument layout** (like generics) — each specialisation reads `type` at that caller's
  offset, preserving sharing at zero new conceptual cost.
- A **stored closure** `(Named) => _` invoked with heterogeneous concrete layouts cannot monomorphise;
  it uses **project-copy at the closure boundary** as the uniform fallback. This is the one spot where
  width-subtyping over anonymous types costs a copy.

### 5.4 Perceus is an optimization here, not a correctness requirement

Reference shares by default, so correctness never depends on move analysis. `rc_elide.rs` still pays:
reuse-in-place for dead record buffers; RC elision around borrows. Upside, never a blocker.

### 5.5 `keys`/`values`/`entries` apply to hashmaps and `AnyVal`, not records (D6)

A record's field set is fixed and known; dynamic enumeration uses a hashmap. `std/object`'s
enumeration narrows to hashmaps/`AnyVal`. One of the four visible changes.

### 5.6 The optional inline-array (value-layout) optimization

Where a `T[]` provably does not alias-escape its elements, lay them out contiguously (Go `[]T`) for
cache locality. Needs escape/uniqueness analysis; deferred; the route to closing the last constant on
Go for scan-dense code.

---

## 6. Implementation plan (staged, gated)

Stageable by value-shape. Reference semantics makes the hardest part of the original plan
(inline-contiguous record arrays) **optional**: the win flows through pointer-backed arrays once
records are flat (Stage 2).

### Per-stage gate

- `cargo build --workspace && cargo test --workspace` — 0 failures.
- `lin test stdlib/ examples/` — full green.
- RAPTOR cross-language digest matches (`group=26203913 range=773022892 journeys=139`) — **expected,
  not guaranteed**: per D5, a stage may *intentionally* change the digest where the old behaviour
  depended on representation-specific aliasing; such a change must be matched by an updated directed
  test, not silently accepted.
- ASan clean; RSS (`VmHWM`) bounded/flat; `records` still beats Go; RAPTOR trends toward Go; `lin fmt
  --check`.

### Stage 0 — Pin the decisions (no code)

- Ratify **D1–D8** (§0.5) in spec + a new ADR (supersede/annotate ADR-062). In particular: the
  reference-semantics + flat-layout decision; the `Json → AnyVal` no-handle union; D3 structural
  parameter monomorphisation + closure-boundary copy; the D4 single-direction `AnyVal` boundary with
  descriptor-carrying as v1; the D5 aliasing-unification (named + directed test); D6 enumeration; D7
  descriptors kept; D8 transitional boxed shadow.
- **Deliverable:** signed-off decisions.

### Stage 1 — All-scalar records: one flat representation, unconditional

- Remove the boxed-shadow arm in `repr.rs` for the all-scalar, **non-`AnyVal`-flowing** case (D8);
  flat struct becomes the representation. Pure internal simplification; lowest risk.

### Stage 2 — Heap-field records: flat struct with constant-offset fields, unconditional

- Const-offset reads of `String`/array/map/nested-record fields as the sole representation
  (non-`AnyVal`-flowing). Pointer-backed collections make `Trip[]` / `{String:Trip}` fast
  automatically. **The big one** — kills the read seam and (with pointer sharing) the regroup cost.

### Stage 3 — Unions of records: tagged value + `match` narrows representation

- `T | Null` → nullable pointer; `A | B` → tag + payload-pointer; narrowing reads payload at known
  layout. Deletes the `Conn`/`Trip | Null` seam.

### Stage 4 — Repoint Perceus/`rc_elide` as a record optimization

- Reuse-in-place for dead record buffers; tidy RC around borrows. Pure upside.

### Stage 5 — Hashmap/array value representation polish

- Confirm `{ String: T }` / `T[]` store record pointers uniformly, reads return the shared record with
  no materialization. Largely falls out of Stage 2.

### Stage 6 — Dissolve `LinObject`; re-found `AnyVal` (the userland migration)

- Object literals default to records; statically-unknown shapes become `{ String: AnyVal }` hashmaps
  or `AnyVal`; `Json → AnyVal` rename throughout. Implement the D4 record-ptr+descriptor `AnyVal`
  boundary; retire the transitional boxed shadow (D8). Delete `object.rs`'s string-keyed storage
  (descriptors kept, D7). Provide the explicit ordered-map (§2.6). Narrow `keys`/`values`/`entries` to
  hashmaps/`AnyVal` (D6). This stage carries the visible-change surface (§7.2); isolating it last keeps
  Stages 1–5 as close to digest-stable as D5 allows.

### Optional later — inline-array (value-layout) optimization (§5.6)

### Closing work

- Update `docs/SPECIFICATION.md`, `docs/STDLIB.md`, `docs/DECISIONS.md`, `docs/PERFORMANCE.md`
  (remove the "inherent PREP copy" caveat — §2.4). Re-measure RAPTOR typed vs Go.

---

## 7. Success criteria, risks, and the breaking change

### 7.1 Success criteria

There is no longer a "typed vs `Json`" race for the same data: typed → record (fast); untyped →
`AnyVal` (slow, by choice). So:

- **Typed RAPTOR approaches Go**, not "approaches the Json port." Expect a small residual constant from
  per-record pointer indirection + per-record allocation; the optional inline-array optimization
  (§5.6) closes it for scan-dense code.
- **`AnyVal` is the only slow path, and only when explicitly chosen.**
- The compiler is **smaller**: `repr.rs` reconciliation, `object.rs` string-keyed storage,
  `BoxKeepPacked`, the path-9 machinery all deleted (descriptors kept, D7).

### 7.2 The breaking change — four userland-visible changes (honest count)

1. **`Json → AnyVal` rename** — mechanical but wide (stdlib signatures, examples, docs). `AnyVal` also
   *narrows* the old `Json` (D2): code that smuggled a handle through `Json` no longer type-checks and
   must keep the value statically typed.
2. **Insertion-order iteration** moves to an explicit ordered container (§2.6).
3. **`keys`/`values`/`entries` no longer apply to records** (D6) — use a hashmap for dynamic keys.
4. **Aliasing unified to share-always** (D5) — `push`-then-mutate is now consistently visible for the
   previously-packed cases.

Note **passing records to functions and in-place mutation through a parameter are unchanged** — the
reference axis (D1) is preserved. (#4 is about the *array/map element* aliasing, not the parameter.)

### 7.3 Risks

- **Large change, but subtractive.** Touches `lin-check`, `lin-ir`, all of `lin-codegen`, and
  `lin-runtime`. Stages 1–5 are *close to* behaviour-preserving (D5 is the named exception), so the
  digest + suite are a strong guard — with the D5 caveat that a digest break may be the intended
  aliasing change.
- **Width-subtyping over anonymous structural types** (D3) is the subtlest correctness area — the
  stored-closure fallback (project-copy) must be applied uniformly or a heterogeneous-layout call reads
  a wrong offset.
- **The `AnyVal` boundary** must stay a single-direction conversion (D4); reintroducing a reconciliation
  oracle re-opens path-9.
- **Pointer indirection / allocation residual** vs Go's inline arrays — mitigated by §5.6.
- **Scope discipline.** Ship stage-by-stage on `master`; interp's call-cost axis is a separate project.

---

## 8. The one-sentence version

Stop representing "a record" as a string-keyed JSON object: make a record a pointer to a **flat packed
struct** with constant-offset access (keeping today's **reference** semantics), make dynamic data a
real hashmap or the JSON-shaped **`AnyVal`** value type (no handles, descriptor-carrying boundary),
delete `LinObject`'s string-keyed storage (keep descriptors), and the typed-vs-Go gap closes as a
*consequence* — with four named userland changes and the parameter-passing behaviour you wanted
preserved.
