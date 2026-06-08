# Path 4 — Split `struct` from JSON object at the surface

**Status:** Open proposal, one of five independent paths. Self-contained.
**Direction in one line:** stop overloading one syntactic construct for two opposite purposes — introduce
a distinct surface `struct` type kind that is fixed-shape, packed (and optionally value-semantic) **by
construction**, leaving JSON objects and `Json` as the dynamic world; the surface tells the compiler
which world you're in, so there is no inference and no gate.

---

## Background (shared context — the problem, the framing, the full history)

### The problem
Reading a field of a known record type, and operating over arrays of such records, is dramatically
slower in Lin than in Go/Rust/Zig/Nim (const-offset loads, const-stride walks there).

### The framing correction (this path *is* the framing made real)
**Lin's type system is not JSON.** It is syntactically JSON-like and shares JSON's primitives, but a
named record type is a known, closed shape — not a dynamic bag. The conflation of "looks like JSON" with
"is represented like dynamic JSON" is the root problem. **This path's whole thesis is to make that
correction true *in the surface syntax*, not merely internally:** every fast language separates structs
from maps (Go `struct` vs `map`, Rust `struct` vs `HashMap`, Zig `struct` vs hash map, TS
interface/class vs index-signature object). Lin has not — internally *or* at the surface — and the entire
painful history below is the consequence of that one construct serving two purposes.

### How Lin represents values today
- **Boxed (default, dynamic):** heap `LinObject`, refcounted, string-keyed; `obj["k"]` a non-inlinable
  `lin_object_get`, opaque to LLVM; the representation of `Json`, inferred literals, subtyped params, and
  every value through a polymorphic stdlib op.
- **Packed / "sealed":** const-offset packed struct; array = header-less `0xFE` buffer + per-field RC
  descriptor; scalar packed field read = `getelementptr + load`, verified.
- **Flat scalar arrays:** `Int32[]` already contiguous + specialized.
- Machinery: `Repr` lattice + oracle (ADR-062); the gate `Type::is_sealed_array_field_packable`
  (scalar+Bool only) — a predicate the compiler uses to *infer* whether a value may pack.

### The three costs
1. **Field reads through the dynamic ABI** — ~72×; fixable, largely solved by a spike for packed.
2. **Operations at the boxing boundary** — dominant: `length(packed Token[])` materializes the whole
   array to boxed `Object[]`; all combinators re-box on entry.
3. **Construction refcounting** — per-element-per-field retain on build + drop-walk.

### The full history (what was tried, learned, failed)
- **H1 — Profile (valid):** typed vs `Json` field read ~72×.
- **H2 — Leaks drained (independent):** RAPTOR ~190 MB/scan → ~97% reduced.
- **H3 — Sealed machinery + harness (sound):** per-field RC, descriptors, keep-packed ops, mechanism (i),
  3-point ASan harness.
- **H4 — Gate widenings net-negative:** scalar→String→Array→Map→nested each found+fixed a real bug
  (silent data loss, a compiler panic, a broad leak, two crashes, missing KIND_MAP), but packing heap
  fields **regressed interp ~3×, crashed the TLV codec, helped RAPTOR nothing**; gate narrowed back to
  scalar+Bool. **Every one of these bugs is a consequence of the compiler trying to *infer* packability
  for a value that is syntactically just an object literal.**
- **H5 — RAPTOR retype: correct, >5× regression**; sub-blockers `get<T,D>` monomorphization +
  `Trip|Null`/`Conn` re-boxing. *(The trips are syntactically objects; the retype is an attempt to coax
  the inference into treating them as structs.)*
- **H6 — The spike:** cheap packed heap-field reads recovered only ~6%; `length`/combinators materialize
  the whole array. Reads weren't the bottleneck.
- **H7 — Ruled out:** boxed inline-slot (unsound under structural subtyping — *because an object's layout
  is not fixed by its type*); shape-ratio gate (3.6× blind spot, *a heuristic for "is this object
  actually a struct"*); cheap-reads-alone; round-key churn (neutral); NaN-box/slab/GC/box-pool.

### The central finding (read through this path's lens)
The bottleneck is the un-integrated combinator ABI (cost #2). But step back: **the deeper reason the whole
effort was hard is that the compiler must *infer* which object literals are really structs, gate whether
to pack them, and reconcile packed-vs-boxed at every boundary** — and structural/width subtyping means an
object's layout is *not* fixed by its type (`{a,b,c} <: {a}`), which is exactly why boxed inline-slot is
unsound (§H7) and why the gate is so delicate. A distinct `struct` kind removes the inference entirely:
you wrote `struct`, it's a struct.

---

## This path's thesis

Introduce a surface **`struct` type kind**, distinct from `type T = {...}` structural objects and from
`{String:T}` maps and `Json`. A `struct` is, **by construction**: fixed-shape, packed (layout already
built — ADR-057/062), read by const-offset, iterated by const-stride, and (optionally, coupling to the
value-semantics path) a value type. JSON objects and `Json` stay exactly as they are, dynamic. **The
surface declares which world you're in — no inference, no packability gate, no "is it a win" heuristic.**

This is the literal implementation of the framing correction: make "the type system is not JSON" true in
the *syntax*, the way every language without this problem did.

- `struct Point { x: Int32, y: Int32 }` (exact syntax TBD) — always packed, always const-offset reads.
- The gate (`is_sealed_array_field_packable`) is replaced by a syntactic fact: *is this a `struct`?* The
  whole §H4 shape-by-shape widening saga becomes unnecessary.
- Flowing a `struct` into a `Json`/dynamic slot **boxes** it at that **explicit, type-level** boundary (a
  `struct → JSON` conversion the programmer can see) — the minimal, visible boundary, not an implicit
  per-read materialization.

## What this path fixes

- **Field reads:** yes, **unconditionally and by construction** — a `struct` is always packed, so a
  `struct` field read is always const-offset. This is the full, *visible* answer to the framing —
  achieved syntactically, not by inference (Path 1's packed-by-default) or speculation (Path 2).
- **Combinator/`length` boundary (cost #2):** yes — **but only if an in-place / monomorphized operation
  ABI is underneath** (Path 1's Step 1). Path 4 supplies clean, closed, statically-known types to
  specialize over; it still needs the *verbs* to operate on them in place. **Path 4 needs Path 1's core.**
- **Construction RC (cost #3):** only if coupled with value semantics (Path 3a) — which a `struct` kind
  is the natural, *non-breaking* carrier for.

## Rationale / why pursue this path

- **It dissolves the inference/gate problem at the root.** No "is this object packable / is it a win" —
  the programmer declared a `struct`. The fragile gate, the §H4 widenings, the §H5 retype coaxing, the §D
  heuristic — all become unnecessary. The hardest parts of the history exist *because* there was no
  surface distinction.
- **It is additive and non-breaking.** Existing `type T = {...}` objects and `Json` are untouched — so
  unlike Path 1's packed-by-default inversion (changes the representation of all records, high blast
  radius) and value-semantics-on-existing-records (breaking), Path 4 introduces the fast world
  *alongside* the dynamic one. Programs opt in by writing `struct`.
- **It matches every fast language** and the framing precisely — the "be like Go/Rust/Zig/Nim" move at
  the surface, where it belongs, not hidden in compiler inference.
- **It is the clean, non-breaking carrier for the two things that actually fix the costs:** value
  semantics (Path 3a) on `struct` only (additive), and monomorphization (Path 1 Step 2b) over closed
  `struct` types.
- **Honest performance model.** The performance cliff is *visible in the type* (`struct` = fast/fixed;
  object/`Json` = dynamic), not hidden behind inference — avoiding the non-locality that sank the
  usage-inferred gate (§H7/§D).

## Cons / risks

- **It is a userland language change** — a new type kind with its own syntax, semantics, and conversion
  rules, requiring real design: how does `struct` relate to a structural object type? can a `struct`
  satisfy an object-typed param (subtyping)? how does `&` intersection (ADR-061) interact? literal
  syntax? pattern matching? mutation (value vs reference — couples to Path 3a)? This is an ADR/spec
  change, then implementation.
- **Two record-like worlds to learn and maintain** — `struct` vs object. A deliberate, documented cost
  (Go/Rust pay it happily), but real surface area Lin doesn't have today.
- **Still needs Path 1's core** (in-place ABI or monomorphization) for the combinator boundary — Path 4
  is the *type*, Path 1 Step 1 is the *operations*. Path 4 alone gives fast field reads but not fast
  `struct`-array `length`/`map`.
- **Migration/ergonomics at the boundary:** the moment `struct` data crosses to `Json` (wire I/O, dynamic
  APIs) it boxes; programmers must understand where `struct`↔object/`Json` conversions happen. Done well,
  explicit and visible (a feature); done poorly, a papercut.

## Relationship to the other paths

- **Path 4 needs Path 1's Step 1** (the in-place / monomorphized operation ABI) to be worth anything for
  arrays — it provides the clean types; Path 1 provides the cheap verbs.
- **Path 4 is the clean carrier for Path 3a (value semantics):** value semantics is only *safe* as an
  additive feature, i.e. on a new `struct` kind. "3a done right" = Path 4 + Path 3a. Together they fix all
  three costs additively.
- **Path 4 vs Path 1's packed-by-default sub-variant:** both make known records fast unconditionally.
  Path 1 does it by *inverting the existing default* (no new surface, but high blast radius / the bug
  class we fought). Path 4 does it by *adding a new kind* (new surface, but additive / nothing breaks).
  Path 4 is the safer way to get "fast by construction."
- **Path 4 vs Path 2 (inline caches):** orthogonal philosophies — Path 2 makes the *dynamic* type fast
  (no new type); Path 4 makes a *static* type the carrier of speed. A system could even have both.

## Acceptance gates

A **language-design pass / spec** first (`struct` syntax, semantics, subtyping & conversion to object &
`Json`, pattern matching, intersection, literal form, mutation/value-vs-reference). Then the shared
implementation gates on top of Path 1's ABI: full `cargo test`; harness + IR-mechanism assertion (no
`sealed_array_to_tagged` in a `struct`-array hot path); RAPTOR digest byte-identical; ASan-clean;
cross-language benchmark non-regression.

## Verdict

The path that fixes the *framing* — make "struct ≠ JSON object" true in the surface, as every fast
language did — giving unconditional, *visible*, fast-by-construction field access **additively** (nothing
breaks), and serving as the clean carrier for value semantics (Path 3a) and monomorphization. It is the
most coherent end-state: it stops the compiler from *guessing* which objects are structs (the source of
the entire painful history). Its cost is a genuine language-design effort and a second record-like world
to learn — and it still needs Path 1's operation ABI underneath. Best if Lin's identity should include
"real structs, fast by construction," and the appetite for a deliberate language-design pass exists.
