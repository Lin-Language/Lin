# Sealed Records — Design Proposal

> Status: **PROPOSAL** (not yet decided/implemented). This document captures the
> design for giving named record types an unboxed, constant-offset struct layout
> while keeping Lin's structural type *compatibility*. It is the outcome of a
> performance investigation into the object representation (see "Background"). It
> is written to be turned into one or more ADRs + spec edits once the direction
> and the open questions are settled.

## 1. Motivation

### 1.1 The measured problem

Lin compiles **every** object — however precisely typed — to the same runtime
value: an opaque pointer to a boxed, refcounted, string-keyed `LinObject`.
Field access `obj["x"]` is a `lin_object_get` hash lookup over interned-key
entries, returning a boxed `TaggedVal*` that must then be unboxed
(`crates/lin-codegen/src/codegen/data.rs:359`). The concrete shape the checker
knows is discarded at the codegen boundary:

```rust
// crates/lin-codegen/src/codegen/types.rs:31
Type::Object(_) => self.context.ptr_type(AddressSpace::default()).into(),
//          ^ the IndexMap<field, type> shape is thrown away
```

This is the dominant cost in object/record-heavy code. The `interp` benchmark
(an AST tree-walker) is ~16–73× slower than native and slower than Python/Node,
and a sequence of measured experiments established the cause is the
**representation**, not the allocator, RC, or box count:

- A free-list box pool was ~3–4% *slower* (glibc tcache already cheap).
- Perceus reuse (FBIP) recycles only the 30% object shells, not the 69%
  payload boxes → sub-1.5× ceiling.
- Eliminating 24% of boxes (string-literal interning) moved wall-time ~1.9%;
  the *same* saving via a HashMap was 8–10% *slower* — proving the cost is the
  per-operation work (the non-inlined call + hash lookup + unbox), not malloc.

### 1.2 The measured ceiling for struct layout

A spike compared identical field-access-dominated logic with the record as
(A) a boxed named-type object `p["x"]` vs (B) a flat `Int32[]` accessed by
constant index `r[0]` (Lin's one existing constant-offset, unboxed
representation — used as a proxy for a sealed scalar record):

| Workload | A: boxed `p["x"]` | B: flat `r[0]` | B faster |
|---|---|---|---|
| Access-only (build once, read 50M×) | ~2460 ms | ~148 ms | **~16.6×** |
| Access + construct (build each iter) | ~5050 ms | ~1726 ms | **~2.9×** |

IR confirms the difference: A emits 4 `lin_object_get` calls per iteration;
B emits 3 inlined constant-offset `i32` loads. This rules out the pessimistic
hypothesis that the cost is "elsewhere": constant-offset access removes the
call, the lookup, and the unbox in one move, and the win is an order of
magnitude on access-bound scalar records.

**Honest caveats on the number:**
- 16.6× is the **optimistic ceiling** (all-scalar, read-only). With
  construction in the loop it drops to ~2.9× because heap allocation + RC is
  representation-independent.
- The flat-array proxy has homogeneous scalar elements, no per-field type tag,
  no heap fields, no per-field refcount. A real sealed record with string/object
  fields keeps refcount traffic and pointer-chasing on those fields, eroding the
  benefit toward the low end.
- **Plan for ~2–3× (and falling for heap-field records), with the high end only
  for scalar-dominated access loops.**

## 2. The core idea: separate compatibility from representation

The insight that makes a struct layout *sound* (an earlier analysis concluded it
was "fundamentally unsound" and was wrong, because it assumed pointer
reinterpretation):

> **Type compatibility stays structural ("has this shape"). Representation
> becomes a per-value guarantee ("is exactly this shape, no extras").** A wider
> value flowing into a named-type slot is *projected* — by a non-mutating copy —
> into a fresh sealed value with exactly the named type's fields. The original
> value is unchanged and keeps its extra fields in its own scope.

Because a wider value is **never reinterpreted in place** as a narrower layout —
it is always *copied into the canonical layout at the boundary* — a value of a
given named type *always* has exactly one known physical layout. The
field-offset ambiguity that would otherwise arise under width subtyping
disappears.

### 2.1 The two halves, by example

```lin
type MyType = { "prop": String }       // a SEALED record type

val wide: Json = { "prop": "a", "extra": "b" }   // a Json with an extra field

val myFunc = (x: MyType) => x["prop"]            // x is a sealed MyType inside here

myFunc(wide)        // ALLOWED — `wide` HAS MyType's shape (structural compat).
                    // At the call boundary, `wide` is projected to a fresh
                    // sealed MyType { "prop": "a" }. `x` has NO `extra` field.
wide["extra"]       // still "b" — `wide` is unchanged outside the call.
```

- **Type checking** is unchanged from today: `wide` is accepted where `MyType`
  is expected because it has at least `MyType`'s fields with compatible types.
  This is the existing rule (`crates/lin-check/src/compat.rs:166`):
  *"value has all target fields with compatible types"* — extra fields permitted.
- **Representation** changes: `x` inside `myFunc` is a sealed `MyType` (struct
  layout, exactly `prop`), produced by a boundary projection. It does not, and
  cannot, observe `extra`.

### 2.2 Decisions taken (user-confirmed)

1. **All named record types are sealed. No opt-in keyword.** `type T = {...}`
   means "exactly these fields" at the representation level. This matches the
   intuition that defining a type names a precise shape, and maximizes how much
   code gets the fast layout automatically.
2. **Boundary coercion is non-mutating and lossy-by-projection.** A wider value
   coerced to a named type yields a fresh sealed copy with only the type's
   fields; extras are dropped *from the copy*, never from the source.
3. **`Type.fromJson(json)` is lossy** in the same way: it validates and projects
   to exactly the type's fields, dropping unknown keys, to uphold the
   no-extra-fields promise.

## 3. Semantics matrix

This is the heart of the proposal: every operation that today assumes the
uniform boxed `LinObject` must have a defined behavior under sealed layout. For
each, "fast" = operates on the struct directly; "boundary" = triggers a
projection/box conversion.

### 3.1 Field read — `x["prop"]` / `x.prop`
- On a sealed value of statically-known type: **constant-offset load** (the win).
  The field's slot index comes from the type's `IndexMap` insertion order.
- Missing-key semantics (spec §6.1: missing → Null) are a *non-issue* for a
  sealed value: the field set is fixed and known, so a key not in the type is a
  **compile-time** error (it already is for a concrete object type), not a
  runtime Null. Dynamic `x[k]` with a non-literal key on a sealed value → must
  fall back (see 3.8).

### 3.2 Boundary coercion (wider/Json → named type) — **the central new operation**
- Fires when a value of a *wider or Json* static type flows into a slot/param/
  return/binding of a named (sealed) type.
- Semantics: allocate a fresh sealed record; for each field of the target type,
  copy (retain, for heap fields) the corresponding field from the source;
  ignore source fields not in the target. Non-mutating: source is untouched.
- This is the generalization of the lossy `fromJson`. It is O(fields).
- **Soundness obligation:** the source must actually *have* the target's fields
  (guaranteed by the type checker for a statically-compatible source). For a
  `Json` source whose runtime shape is unverified, this is exactly the
  `fromJson` validation question — see Open Question 6.

### 3.3 `:Json` coercion (named/sealed → Json)
- A sealed value flowing into a `Json` slot must become a boxed string-keyed
  `LinObject` (the universal Json representation), since downstream `Json` code
  does dynamic key access. This is a **boundary** conversion: materialize the
  struct's fields into a boxed object with string keys.
- Lin already has a `Coerce(from, Json)` instruction (lower.rs `box_to_json`);
  this extends it to build a boxed object from a struct rather than pass an
  existing pointer.

### 3.4 Equality — `a == b`
- Spec §686: equality is structural and order-independent. Must hold *across
  representations*: a sealed `MyType {prop:"a"}` must equal a boxed Json
  `{prop:"a"}` (and a sealed value with the same fields in any order).
- Implementation: sealed==sealed of the same type → field-wise compare by offset
  (fast). sealed==boxed or sealed==different-sealed-type → compare via the
  field/value pairs (one side may need the boxed view, or a shared
  field-iteration protocol). Must NOT become a type mismatch at the LLVM level.

### 3.5 Spread — `{ ...x, "extra": 1 }`
- Spreading a sealed value copies its fields by name into the new literal. If the
  result is itself a named type, the result is sealed; if anonymous/Json, boxed.
- A sealed source spreads its known fields directly (fast enumeration by offset).
  No string-key map needed on the source side.

### 3.6 `keys(x)` and dynamic enumeration
- `keys` of a sealed value returns its type's field names — known at compile
  time. Can be a constant array, or enumerate the struct's fields. No dynamic
  map walk needed.

### 3.7 `toString` / JSON serialization
- A sealed value serializes by emitting its known fields in order. Either a
  per-type generated serializer (fast) or a conversion to the boxed view then
  the existing serializer (simple, slower). Start with the latter.

### 3.8 Dynamic field access `x[k]` (non-literal key)
- A struct has no runtime string→offset map. `x[k]` where `k` is not a compile-
  time-known field of the sealed type → **boundary**: convert to boxed view and
  do the dynamic lookup, OR (cleaner) make this a type error on a sealed value
  (you can't dynamically index a type whose fields are fixed and known). Prefer
  the type error where the static type is a concrete sealed type; fall back to
  boxed only when the static type is `Json`.

### 3.9 Pattern matching / destructuring / `is` / `has`
- `is NamedType` on a `Json` scrutinee: unchanged — validates shape (the
  ADR-054 `lin_matches_schema` path), and now *additionally* the post-match
  narrowed binding is a sealed value (a projection happens at the narrowing).
- `is`/`match` on a value already statically of a sealed union: this is exactly
  the **closed-concrete-union discrimination** already shipped (commit
  `bf6f766`): a StrLit discriminant compiles to a constant-offset field read +
  compare. Sealed layout makes that field read constant-offset too. **The two
  features compose: sealed records + literal discriminant fields give both fast
  layout and fast dispatch.**
- Destructuring `val {prop} = x` reads fields by offset.

### 3.10 Thread transfer (async / share-nothing boundary)
- ADR-043: values are deep-copied across threads via a JSON-shaped tree. A
  sealed value deep-copies field-by-field (its shape is known) into the transfer
  representation, and rematerializes as a sealed value on the other side. No new
  semantic issue; it is another boundary that must handle the struct form.

### 3.11 Arrays of sealed records — `MyType[]`
- The high-value case. A `MyType[]` could be laid out as a contiguous array of
  structs (no per-element boxing) — the same win flat scalar arrays already get,
  extended to records. This is where record-heavy numeric/data code (and the
  cross-language benchmarks) would move. Likely a **phase 2/3** target once
  scalar single records work.

## 4. The performance model (where the win is, where the cost is)

> **Pay a one-time O(fields) projection at the edge of a "sealed island," then
> run with constant-offset access inside it.**

- **Win:** inside any function/scope working with sealed-typed values, field
  access is constant-offset and scalar fields are unboxed. Recursion over sealed
  types (e.g. `evalNode(node: Ast)` where `node["left"]` is itself `Ast`) keeps
  children **already sealed** — no re-coercion on recursive calls. This is
  exactly the AST-tree-walk shape that is slow today.
- **Cost:** each crossing between a sealed type and `Json`/anonymous structural
  types is an O(fields) projection or box conversion. Code that **chatters across
  the boundary** (repeated sealed↔Json round-trips) can be *slower* than today.
  The win is realized when hot code stays inside sealed islands and only coerces
  at genuine entry points (parser output, `fromJson`, IO boundaries).
- **Construction cost is representation-independent** for heap-allocated records:
  the ~2.9× (not 16×) figure is the realistic expectation for construct-heavy
  code; stack-allocating small non-escaping sealed records (a later escape-
  analysis follow-up) would push past it.

## 5. Staged implementation plan

Each stage independently landable, ASan-gated (RC/ownership is the recurring
UAF/double-free bug class; `cargo test` does not catch it — the CI `asan` job
over `examples/*.test.lin` + `stdlib/*.test.lin` is the gate), and verified for
run-equivalence (same observable output as before) over the integration corpus.

**Stage 0 — Decision + spec.** Ratify "all named record types are sealed" and
the non-mutating lossy-projection boundary semantics. Write the ADR(s) and spec
edits (§ on objects, §ADR-048 interaction, narrowing). Pin the semantics matrix
(§3) as the conformance checklist. No code.

**Stage 0.5 — Named-record identity must survive resolution (PREREQUISITE).**
A blocking architectural fact, verified in code: `resolve_named_cycle`
(`crates/lin-check/src/resolve.rs:119-124`) **fully unfolds** a non-recursive
named type annotation (`: Point`) into `Type::Object(IndexMap<field,Type>)` and
*discards the name*. Only *recursive/cyclic* types survive as `Type::Named`. So
by codegen, a `Point`-typed value and an anonymous `{x,y}` literal of the same
fields are byte-identical `Type::Object` values — the only property left to gate
on is "all fields are scalars", which would seal every anonymous literal too
(the broad, RC-heavy, repo-wide variant), not just named types.

To scope struct layout to *named* types (the decided model), named-ness/sealed-
ness must be carried through resolution **without changing structural
compatibility**. Approach (modeled on ADR-051, which makes `StrLit` carry
through as "Str at runtime, distinct at check-time"): mark a record type sealed
when a `type T = { …concrete fields… }` annotation resolves — e.g. a `sealed`
flag (and optionally the name) on `Type::Object`, or a dedicated wrapper —
threaded into `MakeObject.ty`, param/return types, and `temp_types`, reaching
the `MakeObject`/`FieldGet`/`Coerce`/`Release` sites. `compat.rs` continues to
unfold to the field map, so structural compatibility ("has this shape") is
UNCHANGED — only the representation gate keys on the marker. This stage adds the
marker and threads it through; codegen IGNORES it (no representation change yet),
so the **gate is pure run-equivalence**: the entire corpus must type-check and
behave identically, proving the marker is inert until Stage 1 consumes it. The
built-in `Error` alias and other structural aliases are NOT sealed. Risk: this
touches type identity across the checker (compat, field inference, equality,
exhaustiveness, narrowing, zonk) — directly the Open Question 6 corpus risk — so
run-equivalence over `stdlib/`+`examples/`+benchmarks is mandatory.

**Stage 1 — Scalar-only sealed records, single values (the 16× core).**
*Now gated on the Stage 0.5 sealed marker (named + all-scalar), not on
all-scalar-shape, so anonymous literals stay boxed.*
- Codegen: lay out a named type whose fields are *all unboxed scalars*
  (Int32/Int64/Float64/Bool) as an LLVM struct; field read/write =
  constant-offset load/store; construction = field stores by offset. Gate
  strictly to all-scalar concrete named types; everything else keeps the boxed
  path.
- Lowering: insert the boundary projection (wider/Json → sealed scalar struct)
  and the sealed→Json box conversion at the typed edges.
- Equality, `toString`, `keys`, spread for the scalar-struct form (start by
  converting to boxed view for the rare ops; fast-path equality).
- This is the smallest slice that delivers a measurable win and exercises every
  boundary once. Measure on a scalar-record access workload and on `interp`
  rewritten with a scalar-leaf AST.

**Stage 2 — Mixed sealed records (heap fields: String, nested sealed, arrays).**
- Struct slots hold pointers for heap fields, with correct retain/release on
  construct/project/drop (this is the RC-heavy, UAF-prone part — heaviest ASan
  scrutiny). Drop = per-field release by known kind (this is "drop
  specialization" from the earlier RC staircase, now motivated).
- Full equality / spread / serialization / thread-transfer over heap-field
  structs.

**Stage 3 — Arrays of sealed records (`MyType[]` contiguous, unboxed).**
- Contiguous struct arrays, constant-offset element + field addressing. The
  data-heavy win; extends the flat-scalar-array machinery to record elements.

**Stage 4 (optional) — Stack allocation of non-escaping sealed records.**
- Escape analysis to stack-allocate small sealed records that don't escape,
  removing the construction/RC cost that caps Stage 1 at ~2.9× for
  construct-heavy code.

## 6. Open questions

1. **`fromJson` / boundary validation for `Json` sources.** When a `Json` value
   (whose runtime shape is unverified) is projected into a sealed type, do we
   trust the static compatibility (fast, but a malformed Json that the checker
   *thought* was compatible could be mis-shaped) or validate at the boundary
   (the `lin_matches_schema` cost)? Today `Json → concrete` is the unchecked
   ADR-048 sink for non-record coercions, while `fromJson` is the validated
   path. Proposal: a *statically-typed* source (already `{prop, ...}`) projects
   without revalidation; a `Json`-typed source uses the validated `fromJson`
   path. Confirm this split.

2. **Field order / layout canonicalization.** Two `{a, b}` literals constructed
   in different key orders must yield the same sealed layout. Proposal: layout
   order = the *type declaration's* field order (the type's `IndexMap`), not the
   literal's. Equality stays order-independent regardless. Confirm.

3. **Recursive types and sizing.** `type Ast = Num | BinOp` where `BinOp` holds
   `Ast` children: the children are *pointers* to sealed values (a recursive
   type can't be inlined by value). So a sealed record's heap fields include
   sealed-pointer fields. Confirm the layout model: scalars inline, heap/record
   fields by pointer.

4. **Unions of sealed records.** `Ast = Num | BinOp` — the value is one of two
   sealed layouts. Does the union carry a tag, or rely on the discriminant field
   (the shipped `bf6f766` mechanism)? Likely: the boxed/tagged form at the union
   level, sealed layout within each variant once narrowed. Define the
   representation of a *union-typed* value vs a *narrowed* value.

5. **`Error` and built-in structural aliases.** `Error` is a structural alias
   for `{type, message}` (spec §390). Is it sealed? It's used with extra fields
   in practice (`{type, message, ...}`). Likely must stay structural/boxed or
   get special handling. Confirm built-in aliases are exempt.

6. **Migration / corpus impact.** "All named types sealed" changes representation
   repo-wide. The run-equivalence gate must pass over all `examples/`,
   `stdlib/`, and benchmarks. Audit for code that *relies* on a named-typed
   binding silently carrying extra fields through to a later use (the one
   semantic change in §2.2.2). Expected rare, but must be swept.

7. **`has T` vs `is T`.** §795: `has` is "contains at least this shape" (extra
   fields OK), `is` is "presence + types". Under sealed semantics, does `has`
   still make sense on a value already sealed? Probably `has` remains a
   structural probe on Json/wider values; `is` narrows + projects. Confirm the
   interaction.

## 7. Relationship to existing work

- **Composes with closed-concrete-union discrimination (`bf6f766`).** That
  feature already makes `is`/`match` on a StrLit-discriminated sealed union a
  constant-offset field read + compare; sealed layout makes the field read
  itself constant-offset. Pairing sealed records with literal-typed discriminant
  fields is the intended fast idiom for ASTs and tagged data.
- **Motivates "drop specialization"** from the earlier RC staircase
  investigation (per-field release by known kind) — Stage 2 needs it.
- **Does not require Perceus reuse** (measured not worth it for the box churn)
  or the box pool (measured slower). Those remain rejected.
- **ADR-048 (`Json` covariant sink) interaction** is Open Question 1.

## 8. Recommendation

The mechanism is sound and the ceiling justifies the direction for scalar-field,
access-bound records (3–17×). It is a **large** change (a second object
representation plus the full §3 boundary matrix), so it must be staged and
measured at each step, with the realistic expectation set at **~2–3× for typical
record code, higher only for scalar access-bound loops, and a risk of regression
for boundary-chattering code**. Stage 1 (scalar-only single records) is the
smallest slice that both delivers a measurable win and forces every boundary in
§3 to be defined once — it is the right first commit, gated behind a benchmark
that confirms the win on a scalar-record workload before proceeding to the
RC-heavy Stage 2.
