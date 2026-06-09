# Path 5 — Records are values, not refcounted heap objects (the memory-model fix the read paths could not reach)

**Status:** ⚠️ **PREMISE FALSIFIED — see "RESULTS 2026-06-09" at the bottom before reading.** The body
below claims records are *already* value-semantic and so this is "no userland language change." That is
**WRONG**: records are observably-mutable reference types (`val b = a; a["k"]=v` → `b` sees it, verified),
so value records ARE a breaking change. The *cost diagnosis* is sound and confirmed by the RAPTOR profile,
but the non-breaking framing is not — **the live, non-breaking form of "make records cheap" is
[Path 1](path-1-integrate-packed-records.md) (packed-by-default, a representation change), not this path's
value semantics.** Read the body for the cost argument; read the RESULTS section for what's actually true.

**Direction in one line:** stop representing fixed-key records as refcounted heap `LinObject`s and
start representing them as **inline values** (registers / stack / contiguous array slots, no header, no
shell refcount) — the Go/Swift model — because the dominant costs in every measured benchmark
(construction+RC of escaping records, and the boxing boundary at every polymorphic op) **exist only
because records default to the heap**, and *no read-strategy or arena path can remove a cost that the
default representation creates*.

---

## Why this path exists — the premise Paths 0–4 never questioned

All five prior proposals share one unexamined assumption:

> *A record is a refcounted heap object by default. Keep that. Claw performance back around it.*

- **Path 1 (packed records)** fixes the *byte layout* but keeps the heap-reference *model* and keeps the
  dynamic boxed `TaggedVal` ABI as the universal currency — so every `length`/`for`/`map`/`reduce`
  **materializes the packed array back into boxes** on entry (`sealed_array_to_tagged`). It changes how
  the bytes are arranged without changing *what a value is*, so the second representation, and the
  boundary between the two, remain. That boundary (cost #2) is what dominated real programs.
- **Path 2 (inline caches)** makes the heap representation's *reads* fast but leaves every record a
  scattered, pointer-chased, per-object-`malloc`'d, shell-refcounted heap object. It is also explicitly
  "novel for an AOT/LLVM backend, highest implementation unknown."
- **Path 3 / Path 4 (inferred arenas / whole-program regions)** try to dodge per-object `malloc`+RC
  while keeping every value a heap object refcounted across every call boundary. Path 3 then *names the
  real fix and quarantines it*:
  > *"Value/copy semantics (the Go-struct model) would fix all three costs, but it is a breaking
  > semantic change… **Removed from this path.**"*

**That removal is the mistake.** Four agents spent their budget building scaffolding to *simulate the
benefits of value types on top of a heap-reference model* — and the measurements show it doesn't work:
Path 4's decisive H9 result is that typing RAPTOR's trips left reads unchanged and made the program
**2× slower**, because introducing a *second* representation *introduces* a boxing boundary at every
store. You cannot out-engineer the wrong default. The boxing boundary is a tax that exists **only because
there are two representations**; remove the heap-reference default and the boundary doesn't get cheaper —
it ceases to exist.

This path picks up the option Path 3 dropped — and shows it is **not** a breaking change.

---

## The load-bearing claim: Lin's *semantics are already value semantics*

"Value types are a breaking change" is **false for fixed-key records.** Verified against
`docs/SPECIFICATION.md` and the runtime:

1. **Equality is structural, never by identity** (§9). `{ "a": 1 } == { "a": 1 }` is `true`;
   `{ "a":1,"b":2 } == { "b":2,"a":1 }` is `true` (order-independent). There is **no** reference-identity
   operator, and function/iterator/module identity equality is explicitly *undefined*. **So the heap
   identity of a record is not observable by any program.** A value copy is indistinguishable from a
   shared reference under every operation the language offers.
2. **Sealed named records already copy at every type boundary** (§5.9.1). When a wider value flows into a
   `T`-typed slot (param, annotated `val`/`var`, return, `T[]` element) it is *"**copied** into a fresh
   sealed value containing only `T`'s fields."* Lin's spec **already mandates copy semantics** for named
   records — it just currently *implements* that copy as a heap re-box. Value representation is the
   honest implementation of a rule that is already in the spec.
3. **In-place field mutation of a sealed record already crashes** (`stdlib/random.test.lin:326`,
   "BUG 1": `val r: R = {"x":1u64}; r["x"] = 2u64` aborts in `lin_object_set`). It is not a working
   semantic anyone depends on. The idiomatic update is already the spread-copy `{ ...person, "age": 43 }`
   (§3.3) — which *is* value semantics.
4. **The grammar already draws the line** (§5.1 vs §5.1.1). `type Point = {"x":Int32}` (fixed keys) is a
   sealed struct; `{ String: T }` (index signature) is a map; `Json` is the dynamic bag. Every prior path
   *noticed* this distinction but used it only to choose a byte-layout — never to choose a *memory model*.

**Therefore: making fixed-key records true value types is semantics-preserving.** The only corner where
reference-aliasing could leak into observable behavior is in-place field mutation through an alias
(`val b = a; a["k"] = v` — does `b` change?) — and that (a) already crashes on sealed records and (b) is
used in the stdlib only on `{String:T}` *maps*, which stay on the heap under this path.

---

## The redesign — one representation rule, drawn on the line the grammar already carries

| Surface form | Today | This path |
|---|---|---|
| `type Point = {"x","y"}` (fixed keys) | refcounted heap `LinObject` | **inline value** — fields in registers / stack / inline slots, no header, no shell refcount |
| `Point[]` | boxed `Object[]`, or packed-but-re-boxed at every op | **`N × sizeof(Point)` contiguous buffer** — *the* representation, not a second one to convert to |
| `{ String: T }` (index signature) | heap map | heap map — **unchanged** (genuinely dynamic; arbitrary keys) |
| `Json` / undiscriminated union | boxed `TaggedVal` | boxed `TaggedVal` — **unchanged** (the dynamic escape hatch; fine if slow) |

### How this dissolves all three measured costs at once — the thing no single path could do

- **Cost #1 (field reads):** inline field = `getelementptr + load`. By construction, for every concrete
  record, not behind a guard (cf. Path 2) and not only when "already packed in a register" (cf. H1).
- **Cost #2 (the boxing boundary — the dominant real-program cost):** *eliminated, not optimized.* There
  is no second representation for `length`/`for`/`map`/`reduce`/`sort` to materialize into. They operate
  on the contiguous value array directly: `length` is a header load; iteration is a const-stride walk
  handing the callback a borrowed interior pointer whose typed param makes its field reads const-offset.
  This is the cost that made the H5/H9 typed retype *regress*; it disappears because there is no longer a
  packed-vs-boxed pair to disagree.
- **Cost #3 (construction + RC — the cost Paths 1/2 structurally cannot touch, Path 4's whole reason to
  exist):** a value record is not heap-allocated and its *shell* carries no refcount. RAPTOR's
  build-once / escape-the-frame / program-lifetime trips become a contiguous value array — **zero
  per-object `malloc`, zero shell retain/release, one bulk free of the backing buffer**. Path 4's
  expensive whole-program region *analysis* is largely unnecessary because there is no per-object heap
  allocation to region away in the first place. (Path 4's own H9 LOAD −38% — "typing the loader's
  construction was the one thing that got faster" — is a direct preview of this win.)

### The ABI is well-trodden, not novel
Small values in registers; large values via a caller-allocated `sret` pointer; arrays as contiguous
`N × stride`. This is the C / Go / Swift / Rust calling convention, native in LLVM/inkwell
(`byval`/`sret` attributes). It is **less** exotic than Path 2's runtime-warmed inline caches — the one
path flagged as "novel for AOT, highest unknown."

---

## The mutation question (the one observable corner — and why it's a zero-regression close)

The user-facing decision is what `r["k"] = v` means on a sealed record:

- **Decided: it becomes a compile error**, steering to the already-idiomatic spread-copy
  `{ ...r, "k": v }` (§3.3). This is **zero regression** — it *crashes today* (BUG 1) — and makes the
  value model trivially sound (no alias can observe a mutation, because mutation through an alias is not
  expressible). In-place index assignment remains valid on **arrays** (§27.1, `packet[1] = 42u8`) and on
  **`{String:T}` maps** (which stay heap), exactly as today.
- **Deferred (a later, optional optimization, NOT a prerequisite):** restore `r["k"] = v` syntax via
  copy-on-write on a uniquely-owned value (Swift's `isUniquelyReferenced`, or Perceus FBIP — the codebase
  already carries reuse/RC-elision machinery). Sound because a *unique* value has no aliases to observe
  the write. Purely additive; specify and build only if the spread-copy idiom proves ergonomically
  insufficient.

`var`-capture-by-reference (§6.2, ADR-012) is **orthogonal and unchanged**: that is a mutable *slot* (a
heap cell shared by closures), which now holds a *value* instead of a box. The capture mechanism does not
change.

---

## The hard parts — all *known territory*, and each reuses machinery already built

This is not free. But the difficulties are engineering with precedent, not research:

1. **Heap fields inside value records — the Swift model.** `type Person = { "name": String,
   "friends": Person[] }`: the *shell* is inline; `name`/`friends` are pointers to refcounted heap things.
   Copying the value does a shallow per-field **retain of the heap-field pointers on an actual
   copy/escape** — *not* a per-object box on every op. **The per-field RC descriptor machinery from H3 is
   already built and merged** (`elem_desc`, the drop-walk, `clone_sealed_array`). It stops being a
   "boxing-boundary witness" and becomes the value type's copy/destroy witness. Reuse, not new code.
2. **Recursive records cannot be fully inline** (infinite size). `type Ast = { "kind", "left": Ast,
   "right": Ast }`: the recursive *edges* are boxed indirections (Rust `Box`, Swift `indirect`). **This is
   exactly the in-flight `project_sumtype_build` work** (unboxed value shell + boxed recursive child
   edges, Stages 0–2 already merged). It composes directly.
3. **The single remaining boxing boundary is at the dynamic edge, where it belongs.** A value record
   flowing into a `Json` slot or an undiscriminated union materializes into a `LinObject`. This is the
   *inverse* of today's economics: today *everything* is boxed and you pay to escape *into* packed on
   every op; here *everything* is a value and you pay to box *only at the `Json`/union cast you explicitly
   wrote*. The boundary moves from "every `length` call" to "the dynamic cast in the source."
4. **Width / structural subtyping** (`{a,b,c}` passed where `{a}` is expected) — Path 1's headline risk —
   is **already solved by the spec's mandated projection-copy** (§5.9.1). You never reinterpret a wider
   layout through a narrower static type; you project-copy into a fresh narrow value, which the spec
   *already requires*. The value model makes the existing semantics *cheaper to implement*, not riskier:
   the project-copy that is today a heap re-box becomes a field-subset value copy.
5. **The repr-inference pass already exists** (ADR-062, `crates/lin-ir/src/repr.rs`: lattice + oracle +
   verifier). Today it decides packed-vs-boxed *of a heap value*. Under this path it decides
   inline-value-vs-boxed and inserts the *one* materialization at genuine dynamic boundaries. The
   structural guard infrastructure for "did we get the boundary right" is built.

---

## Relationship to the prior paths — this reframes them, it does not merely compete

- **Path 1** is the *layout half* of this path minus the memory-model change that actually kills cost #2.
  This path is "Path 1 taken to its honest conclusion: the packed thing **is a value**, not a
  refcounted-heap object that happens to be laid out packed and re-boxed at every verb."
- **Path 4's whole-program region inference** narrows from *the* load-bearing fix to a *targeted* tool for
  deep heap-field graphs (recursive ASTs, long-lived string-heavy records) — because value records never
  hit the heap, there is far less per-object allocation left to region away.
- **Path 2 (inline caches)** survives only as an optimization for the genuinely-`Json` remainder — which
  is correct: dynamic data is the only thing that *should* need dynamic-dispatch tricks.
- **Path 3's arenas / escape-RC-elision** compose for the residual heap-field graphs, but are no longer
  on the critical path for the scalar-record majority.

---

## Reconciliation with the measured history (H1–H11) — so this doesn't repeat the agents' errors

- **H1 (typed read ~72×)** — real but measured an *already-in-register* value; this path makes *every*
  concrete-record read that fast by construction, which is what H1 actually wanted.
- **H4 (packing heap fields regressed interp ~3×, crashed TLV)** and **H5/H9 (typed RAPTOR retype 2×
  slower)** — both were the **cost-#2 boxing boundary** between a packed representation and the dynamic
  ABI. This path *removes the second representation*, so the boundary that caused those regressions does
  not exist. **This is the direct rebuttal to the result that killed the typed retype.**
- **H6 (cheap reads recovered only ~6%)** — confirms reads were never the bottleneck; this path does not
  rest its case on reads. It rests on dissolving cost #2 and eliminating shell-RC/alloc (cost #3).
- **H8 (call-boundary RC ~13× cliff)** — a value passed by `byval`/`sret` carries no shell refcount across
  the boundary, so the cliff for the value-shell disappears (heap *fields* still retain on genuine
  escape, which is correct and minimal). The inlining/fusion lever (already merged) composes on top.
- **H11 (dominant cost = escaping, program-lifetime small records)** — exactly the records this path makes
  into contiguous value arrays with zero per-object alloc/RC.

---

## Staged migration — each stage ships and is independently benchmarkable

1. **Stage 1 — by-value ABI for all-scalar fixed-key records.** The `0xFE` packed buffer is *already the
   right bytes*; give it a by-value calling convention and make `length`/index/`for`/`map`/`filter`/
   `reduce`/`sort` operate on it **in place** (stop emitting `sealed_array_to_tagged`). Make
   sealed-record `r["k"]=v` a compile error (steer to spread). **Expected: turns the H5/H9 RAPTOR
   regression into a win for the scalar case, because it removes cost #2 where the data is already
   packed.** Re-measure RAPTOR (LOAD/PREP/GROUP/RANGE, digest byte-identical) + interp + the
   cross-language suite. This stage is the cheapest test of the entire thesis.
2. **Stage 2 — heap-field value records (Swift model).** Inline shell + pointer heap fields; per-field
   retain-on-copy via the existing H3 descriptor machinery; the *one* materialization at `Json`/union
   boundaries via the ADR-062 repr pass. Re-widen `is_sealed_array_field_packable` from a perf heuristic
   to a soundness predicate ("can this shape be a value, or does it reach a genuine dynamic boundary").
3. **Stage 3 — recursive records.** Value shell + boxed recursive edges, composing with
   `project_sumtype_build`. Port the interp `Ast` and measure (the payoff gate that path defined).
4. **Stage 4 — `Json`/union boundary materialization** hardened as the single explicit dynamic-edge box,
   with the repr oracle/verifier covering every site (the §H4/H5 mismatch class lives *only* here now).
5. **Stage 5 (optional) — CoW mutation** to restore `r["k"]=v` if the spread idiom proves insufficient.

---

## What this path fixes

- **Field reads (#1):** yes — const-offset by construction, no guard.
- **Boxing boundary (#2):** **eliminated** — no second representation to convert to/from.
- **Construction + RC (#3):** **yes** — value records aren't heap-allocated and shells aren't refcounted;
  heap *fields* retain only on genuine copy/escape. The first path to fix #3 *without* a whole-program
  analysis.
- **Call-boundary RC cliff (H8):** the value-shell half disappears (`byval`/`sret`, no shell RC); composes
  with the merged inlining/fusion lever for the rest.

## Cons / risks

- **It is the biggest representation change of any path** — it changes what a fixed-key record *is*, end
  to end (ABI, codegen, RC emission, the repr pass's decisions). Mitigated by the staged migration: Stage
  1 alone is a contained, falsifiable test of the whole thesis on already-packed bytes.
- **Heap-field copy semantics (Stage 2)** must get per-field retain-on-copy exactly right — the recurring
  UAF/double-free class. Mitigated: the H3 descriptor machinery + ASan harness already exist and already
  caught a `sort` leak manual probing missed.
- **The dynamic boundary (Stage 4)** is the one remaining packed/boxed-mismatch surface — every `Json`/
  union/FFI/transfer/`toString` edge must materialize. Same risk Path 1 carried, but now confined to
  *one explicit place* (the dynamic cast) instead of *every polymorphic op*.
- **CoW (Stage 5)** is real work if the spread idiom proves insufficient — but it is optional and
  deferred, not on the critical path.

## Acceptance gates (every stage)

Full `cargo test --workspace` green; the sealed harness green **plus the IR-mechanism assertion** (no
`sealed_array_to_tagged` / per-element box in a typed combinator/`length` hot path — H4 proved
correctness-green is insufficient); RAPTOR digest byte-identical
(`group=26203913 range=773022892 journeys=139`); ASan-clean (no UAF/double-free/scaling leak — the
heap-field copy contract and the dynamic boundary are the risk surfaces); **cross-language benchmark
non-regression by median** (interp, RAPTOR GROUP/RANGE, records, dijkstra) — prove the mechanism in IR
**and** the wall-clock.

## Verdict

The four-agent failure was not in execution — it was in accepting "records are heap objects by default"
as fixed and trying to optimize around the costs that default *creates*. Lin's own semantics
(structural equality, mandated copy-on-boundary for sealed records, a grammar-level struct-vs-map split,
and an in-place-mutation that already crashes) say records were *designed* to be values; the
implementation simply never honored it. Honoring it dissolves cost #1 (reads), cost #2 (the boxing
boundary that regressed every typed retype), and cost #3 (construction/RC that no read path can reach) in
a **single, semantics-preserving** change, with an ABI every systems language already uses and machinery
(H3 descriptors, ADR-062 repr pass, `project_sumtype_build`, the inlining lever) already built. Stage 1
is the cheap, falsifiable first move. **This is the fundamental redesign the benchmarks have been asking
for.**

---

## RESULTS 2026-06-09 — the central premise is FALSIFIED (records are observably-mutable references), but the diagnosis it pointed at is CORRECT

**Verdict: this path's thesis — "Lin's records are *already* value-semantic, so making fixed-key records
inline values is semantics-preserving / no userland change" — is FALSE. Verified directly, not
self-reported.**

### The falsifying test (run on master, byte-checked)
```txt
type Counter = { "state": Int32, "inc": Int32 }
val a: Counter = { "state": 0, "inc": 7 }
val b = a
a["state"] = 99
print(toString(b["state"]))   // → 99
```
`b` sees `a`'s mutation. Records are **genuine mutable reference types with observable aliasing** — through
plain `val b = a` (the simplest case), and through function-argument passing (the existing, *passing* test
`test_sealed_record_field_write_through_helper`, integration.rs:7275, whose comment states outright: *"a
sealed record is a mutable reference like an array — the mutation must be visible at the call site"*).

### Where the proposal's argument went wrong
- **Structural equality (§9) does not imply value semantics.** §9 means a *copy* is indistinguishable from
  a *reference under reads*; it does **not** mean *mutation* is unobservable — and aliased mutation plainly
  is. The proposal conflated "no identity equality" with "no observable identity." They are different.
- **§5.9.1's "copy at the boundary" is only *projection*** (width-narrowing a wider value into a *smaller*
  named type). A same-type `val b = a` does **not** copy. So the spec does not already mandate value copies
  for the aliasing case the model needs.
- **"BUG 1 — `r["k"]=v` already crashes" is STALE.** Verified: in-place packed-sealed-record field write
  works today and is tested. The "zero-regression" justification for making index-assign a compile error
  rested on a bug that is already fixed; doing so would BREAK `test_sealed_record_field_write_through_helper`.

### Consequence: value records ARE a breaking semantic change
Making records value types changes mutation/aliasing/identity — exactly what Path 3 said when it placed
value semantics out of scope, and exactly the constraint this path claimed to dodge. **It cannot be done
"semantics-preserving."** The user's literal ask ("Go's approach, everything inline on stack") *is* value
semantics, so the breaking change may be *acceptable* — but it is a **language-direction decision**, not a
free implementation swap, and must be made explicitly (and would need a migration: the `makeCounter`
in-place idiom, the helper-mutation test, any `obj[k]=v`-through-alias code). See
[[project_records_are_reference_types]].

### What survives — the diagnosis, redirected
The proposal's *cost* analysis was right; only the "non-breaking" claim was wrong. The
[RAPTOR profile](path-8-make-functions-free.md) then confirmed the value-records *target* is real for the
`Json`-read-bound case: **631 M of 756 M `lin_object_get` are linear scans + ~3.5 B box ops** — the boxed
representation IS RAPTOR's bottleneck. And **partial typing REGRESSES ~13%** (materializes a fresh sealed
struct per access on top of the still-boxed source), which is the measured proof of this path's own warning
that the win is *end-to-end or nothing*. So the representation lever is correct; the open question is the
**non-breaking** way to get it — which is **Path 1's packed-by-default (a representation change, not a
semantic one)**, not this path's value semantics. **Path 1, not Path 5, is the live form of "make records
cheap."** This path stays on file as the considered-and-rejected value-semantics option: rejected not
because it wouldn't be fast, but because it is breaking and Path 1 reaches most of the same layout benefit
without changing what a program means.

### If pursued anyway (the only sound framing)
Opt-in, additive value types (a distinct `struct`-like declaration / explicit value-record marker) for
hot leaf data only, leaving today's reference records unchanged — the non-breaking subset. That is recorded
as "Layer 2a" in [Path 7](path-7-tracing-gc-foundation.md). It is additive, not the wholesale default-flip
this path originally proposed.
