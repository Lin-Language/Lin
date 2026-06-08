# Path 3 — Change the memory model: records as values (and/or arenas)

**Status:** Open proposal, one of five independent paths. Self-contained.
**Direction in one line:** the languages we're chasing are fast because small records aren't refcounted
heap objects — they're values, inline in their containers, copied, with no per-field RC. Attack the
*memory model* (the construction-RC cost the read-focused paths can't touch), via value semantics
(the Go-struct model) and/or arena allocation, with static ownership noted as the far endpoint.

---

## Background (shared context — the problem, the framing, the full history)

### The problem
Reading a field of a known record type, and operating over arrays of such records, is dramatically
slower in Lin than in Go/Rust/Zig/Nim (const-offset loads, const-stride walks there).

### The framing correction
**Lin's type system is not JSON.** A named `type` is a known, closed shape, not a dynamic bag. The
"looks like JSON → represented like JSON" conflation is the root problem. *This path's reading of the
framing:* the fast languages aren't merely *laid out* better — their records are *a different kind of
thing* (values, not refcounted heap objects). The performance gap is partly a **memory-model** gap, not
just a representation one.

### How Lin represents values today
- **Boxed (default):** heap `LinObject`, refcounted, string-keyed; `obj["k"]` a non-inlinable
  `lin_object_get`, opaque to LLVM; the representation of `Json`, inferred literals, subtyped params, and
  every value through a polymorphic stdlib op.
- **Packed / "sealed":** const-offset packed struct; array = header-less `0xFE` buffer + per-field RC
  descriptor; scalar packed field read = `getelementptr + load`.
- **Flat scalar arrays:** `Int32[]` already contiguous + specialized.
- Machinery: `Repr` lattice + oracle (ADR-062); gate `is_sealed_array_field_packable` (scalar+Bool only).

Crucially: **both** representations are **refcounted heap values.** Even a perfectly packed record pays
per-object heap allocation + per-field retain on construct and a descriptor drop-walk on free.

### The three costs
1. **Field reads through the dynamic ABI** — ~72×; fixable, largely solved by a spike for packed.
2. **Operations at the boxing boundary** — dominant: `length(packed Token[])` materializes the whole
   array to boxed `Object[]`; all combinators re-box on entry.
3. **Construction refcounting** — per-element-per-field retain on build + drop-walk on free. **This is
   the cost Path 3 targets and the one Paths 1/2 structurally cannot fix.**

### The full history (what was tried, learned, failed)
- **H1 — Profile (valid):** typed vs `Json` field read ~72×; measured already-packed reads.
- **H2 — Leaks drained (independent):** RAPTOR ~190 MB/scan → ~97% reduced; bench completes. *(Note: the
  RC machinery this fought is the same machinery Path 3 would partly remove.)*
- **H3 — Sealed machinery + harness (sound):** per-field RC, descriptors, keep-packed ops, mechanism (i),
  ASan harness.
- **H4 — Gate widenings net-negative:** scalar→String→Array→Map→nested each found+fixed a real bug;
  packing heap fields regressed interp ~3×, crashed TLV, helped RAPTOR nothing; gate narrowed back.
- **H5 — RAPTOR retype: correct, >5× regression**; sub-blockers `get<T,D>` + `Trip|Null`/`Conn` re-boxing.
- **H6 — The cheap-packed-*reads* spike:** making packed heap-field *reads* cheap (the
  `BoxedArrayFieldGet`/direct-const-offset-read spike) recovered only ~6% on interp; `length`/combinators
  still materialize the whole array. Reads weren't the bottleneck — **and that spike explicitly could not
  touch the remaining cost, which includes per-element-per-field retain on construction (#3).** (A *read*
  spike — do not confuse with H8.)
- **H7 — Ruled out:** boxed inline-slot (unsound); shape-ratio gate (3.6× blind spot); cheap-reads-alone;
  round-key churn (neutral); NaN-box/slab/GC/box-pool (prior negatives — note these were *allocator*
  tweaks; Path 3 is a *semantic/lifetime* change, a different axis).
- **H8 — The function-boundary cliff (solidly measured) + a read-only-arg-borrow spike (the *fix* for it
  — built but UNSOUND, see correction):** a known-type record built+read **inline** is ALREADY free — escape analysis stack-allocs it +
  suppresses its RC (live on master): a 200M-iter inline loop = **0.74 s** = scalar-floor parity, zero
  RC, alloca→registers. But passing that same record **to a function** = **9.7 s** — a ~13× cliff,
  *identical with RC-suppression on or off* — because `escape.rs` conservatively treats every call
  argument as an escape. Since Lin is functional (everything, incl. `for`, is a call), the inline value
  win evaporates at the boundary. **A prototype interprocedural read-only-non-escaping-arg summary (a
  borrow, no RC — the compiler-internal `&T`, *inferred*, no borrow-checker surface) was built** to
  recover it (gate `LIN_BORROW_ABI`, on `worktree-agent-a34be6299838674d9`). **⚠️ CORRECTION (verified
  end-to-end): this prototype is UNSOUND on the target shape — its "sound, ~1.5×, 948 tests clean"
  claim does not hold and must not be relied on.** Findings, reproduced directly:
    - **It produces WRONG VALUES.** With the gate on, a trivial program (build `Pt={x:i,y:i*2}` in a
      `.for` loop, sum `dist(p)`, expected `30` at N=5) prints deterministic garbage (`-1491889465` /
      `-5858785455` / `-9996660890`, varying per build = reading uninitialised memory); the 50M bench
      gives garbage vs the correct `3749999925000000`. Gate off is correct.
    - **Root cause (IR):** the caller constructs the stack record in the BOXED/object layout (`store ptr
      %box…` at 8-byte strides) while the borrowed callee reads it as a PACKED scalar struct (`load i32`
      at const offsets) → it reads box-pointer bits as integers. The spike's premise *"no codegen
      changes needed"* **is** the bug: stack-allocating for a borrowed call requires construction to
      emit the packed layout the callee reads, which it does not.
    - **Why the green suite is NOT evidence of soundness** (the load-bearing nuance): instrumenting the
      borrow-apply site shows it fires **0 times across all 676 integration tests** and 96 times in the
      corpus — and the corpus passes because those 96 are simple top-level shapes that round-trip. The
      break is shape-specific: a record built+passed **at top level** is correct (`7`), but a record
      built **inside a `.for` closure body** then passed is garbage — and the in-closure shape is exactly
      the interp/RAPTOR hot loop this optimisation targets. No test exercises it, and ASan is blind to it
      (a wrong-*value* / wrong-layout-read bug reads valid-but-wrong bytes, not a memory-safety
      violation). So "948 tests + ASan clean" proves only that the gate never fired on tested code.
  The borrow **direction** remains plausible and is still the thesis of this path — but it is **not yet
  proven**; the correct construction-side codegen (emit the packed layout the borrowed callee reads) is
  the unsolved core, not a detail. Walls noted by the spike (`Direct`-callees-only; indirect/closure
  `.for(fn)` + cross-module follow-on) stand, but are moot until the layout-at-construction bug is
  fixed. *(The read-only-**arg** spike — distinct from
  H6's read spike. An earlier draft conflated the two and cited H6's ~6% as if it were this result; and
  the prototype's own "~1.5×" boundary figure is unreliable — it was measured on a run that returns wrong
  values, so no speedup number from this prototype should be quoted until the layout bug is fixed and the
  output is value-verified.)*

### The central finding
The packed representation isn't integrated with the verbs (cost #2 — Path 1's domain). **Separately,
every value stays a refcounted heap object (cost #3) — which no read/dispatch path addresses.** Two
independent spikes pin this: the cheap-*reads* spike (H6) recovered only ~6%, proving reads are not the
bottleneck and the remainder is the boundary (#2) + construction/RC (#3); and the read-only-arg-borrow
spike (H8) **identified** the construction/RC cost at the *function-call boundary* — the ~13×
inline-vs-funcarg cliff that dominates a functional language where everything is a call. The inline-vs-
funcarg cliff itself is solidly measured (a known-type record is free inline, ~13× worse passed to a
function, because `escape.rs` treats every call-arg as an escape). The borrow *fix* for it, however, is
**not yet proven**: the H8 prototype returns WRONG VALUES on the construct-in-closure hot-loop shape (a
caller/callee layout mismatch — see the H8 correction above), and its "sound / ~1.5×" headline does not
hold. So the **problem** H8 pins (boundary RC is the cliff, and it's removable *in principle* the way
Go/Rust avoid it — passing a value is a borrow/copy, not per-object RC) stands; the **solution** still
needs the construction-side codegen built and value-verified. Go/Rust are fast not only because of
layout but because a small record is a value with no per-object RC — the open question this path must
still answer is whether Lin can adopt that at the boundary *correctly* on its existing runtime.

---

## This path's thesis

Stop making refcounted heap objects faster; make small records **not be refcounted heap objects.** This
is the qualitative difference between Lin's model and Go's. Three sub-options on the memory-model axis,
from most-targeted to most-extreme:

### 3a — Value / copy semantics (the Go-struct model) — the primary proposal
**No new syntax — the seal/hash distinction the compiler needs already exists at the surface.**
`type Person = { "age": UInt8 }` (fixed string-literal keys) is a **sealed, fixed-shape record**;
`type MyHash = { String: UInt8 }` (an index signature) is a **hashmap**. They are already distinct
types the checker tells apart — so value semantics attaches to the record kind the syntax *already*
identifies. There is no `struct` keyword, no annotation, no userland addition: a sealed record type is
the value type, an index-signature/`Json`/unsealed object is the dynamic, heap-backed kind.

A sealed record gets **value semantics**: assignment/argument-passing copies the bytes; the value lives
**inline** in its container (a `Token[]` is `Token` structs back-to-back; `{a: Token}` stores `Token`
inline). **No heap allocation per record, no refcount, no `LinObject`.** Heap fields *inside* a value
record (String/Array) still need ownership on copy/drop (copy = retain inners or copy-on-write; drop =
release inners) — the descriptor-walk machinery we have, run on copy instead of construct.

- **Fixes:** reads (inline → const-offset), the boundary (a value array is contiguous; `length`/iterate
  are like a scalar array today, no boxing), **and construction RC (uniquely)** — a small record costs
  zero heap alloc + zero refcount.
- **The one thing to settle — value vs reference is a *behavioral* (not syntactic) decision.** Today
  sealed records are *reference* values (mutation through one binding is visible through aliases;
  identity; closures capture the reference, ADR-012's `var`-capture-by-reference). Value semantics
  changes that: assignment copies, mutation is local, aliasing disappears. This is settled **without new
  surface** — by deciding the semantics of the *existing* sealed-record type — and the safest build-out
  reaches it **compiler-internally and inferred** (S1–S5 below): the escape/borrow analysis copies or
  borrows under the hood with **no observable semantics change** until/unless a deliberate decision is
  made to expose value-mutation semantics. (`var`-mutated records, and records whose identity/aliasing
  is observed, stay reference/heap by the same escape analysis — so existing programs are unaffected.)

### 3b — Arena / region allocation — the targeted, semantics-preserving sub-option
Much construction cost is per-object alloc + per-field RC for graphs with a **bounded, common lifetime**:
RAPTOR's loader builds *all* trips once, the scan reads them, all freed together; interp builds a token
stream, parses it, discards it. Allocate such graphs in a **region** (bump-allocate, no per-object RC,
free the whole region at once).
- Entry: an explicit scoped `region { ... }` / `withRegion`, **or** compiler-inferred regions via escape
  analysis (Lin already has closure-cell escape analysis, ADR-012 — the same analysis identifies
  region-confined record graphs); values that escape are promoted/copied out at the boundary.
- **Fixes:** construction RC (cost #3) — the build phase becomes a bump-pointer increment + one bulk
  free. **Does *not* fix reads/boundary by itself** — a region-allocated *boxed* record still reads via
  `lin_object_get`; arenas are *orthogonal* and compose with Path 1/2 (which fix reads) or with packing
  (a packed region = contiguous + RC-free).
- **Lower-risk than 3a:** it changes *where/how* values are allocated/freed, **not** value/reference
  semantics — transparent to program meaning. Risk is escape-analysis soundness (a missed escape → UAF
  on region drop) and, for the explicit form, a (small) surface addition.

*(A static ownership / borrow-checker model — Rust's — is the logical far endpoint of "no GC, no RC",
but it is a wholesale userland language redefinition (lifetimes, `&`/`&mut`, moves) opposed to Lin's
small/JSON-friendly identity, and is **excluded**. The axis this path lives on is: RC (today) → inferred
value/borrow (3a, compiler-internal) → arena (3b). The borrow analysis 3a uses is *inferred and
invisible* — it is emphatically not a user-facing borrow checker.)*

## What this path fixes

- **3a (value semantics):** reads + boundary + **construction RC** — all three.
- **3b (arenas):** **construction RC** only (composes with another path for reads/boundary).

## Rationale / why pursue this path

- **It is the only path that addresses construction RC (cost #3)** — the cost the read-focused spike
  explicitly could not touch, and which the ~6% read-recovery implies is a large remainder. For
  build-heavy workloads (RAPTOR's loader, interp's tokenizer — both textbook build-once/read-many), this
  is *the* cost.
- **3a is the model the target languages actually use.** "Be like Go/Rust/Zig/Nim" is fundamentally a
  value-semantics statement, not merely a packed-layout one. This path takes the framing at its deepest.
- **3b is the lowest-risk way to claw back construction cost** without any semantic change, reusing
  existing escape analysis, and matches the workloads' build-once/read-many shape exactly.

## Cons / risks

- **3a is a behavioral change to the sealed-record kind (reference → value) — but NO new surface.** The
  sealed-vs-hash distinction is already syntactic (`{ "k": T }` vs `{ String: T }`), so no keyword is
  added. The risk is in the *semantics* (assignment copies, mutation is local, aliasing/identity change)
  and in getting the escape/borrow analysis sound — which is why the build-out is **inferred and
  staged** (S1–S5), keeping observable behavior unchanged (records whose mutation/identity/aliasing is
  observed stay reference/heap) until any deliberate exposure of value-mutation is decided. Copy cost
  replaces alloc/RC cost — needs an internal by-reference path and possibly CoW for heap fields.
- **3b** is orthogonal — it does nothing for reads alone, so it only pays off *composed* with a path that
  makes reads cheap (1/2) or with packing. Escape-analysis soundness is a UAF risk; explicit `region`
  (if pursued) is a small surface addition — the *inferred* region form has none.
- Heap fields inside value/arena records still need RC discipline on copy/drop — Path 3 removes the
  *shell* RC, not the inner-pointer RC (unless the whole graph is in a region).

## Relationship to the other paths

- **3a applies to the existing sealed-record type — no new kind needed.** The syntax already separates
  sealed records (`{ "k": T }`) from hashmaps (`{ String: T }`); 3a gives the sealed kind value
  semantics, inferred and staged, with no userland addition.
- **3b composes with everything:** arenas fix construction; Path 1/2 fix reads; together they cover all
  three costs. The lowest-total-risk "fix all three costs" combination is plausibly **Path 1 (in-place
  ABI) + Path 3b (arenas)**.
- **3a/3b need a reads-cheap mechanism too:** value records still get read — 3a's inline values are
  const-offset by construction; 3b's region records need Path 1/2 unless also packed.
- **Orthogonal to Path 2** (which leaves records refcounted heap objects and makes their *reads* fast):
  Path 2 + Path 3b = fast dynamic reads + cheap construction, no new type, no value semantics.

## Implementation plan for 3a (the read-only-arg-borrow build-out)

3a does not have to land as a big-bang language change. The H8 spike was the first stage of a staged,
each-step-ASan-gated build that reaches value semantics incrementally on the existing static-RC runtime
— **inferring** the borrow (no user-facing borrow checker, no new syntax, no spec change — it applies to
the sealed-record type the syntax already identifies). The escape pass + RC-suppression + sealed packed
layout + the `Repr` lattice and its `verify`/`oracle_check` gates already exist; the new work is the
interprocedural summary and a by-borrow ABI — **with the construction-side codegen the H8 prototype got
wrong (it returns garbage; see the H8 correction) as the actual unsolved core of S1.**

| stage | scope | risk | gate |
|---|---|---|---|
| S0 | (prereq, shared w/ Path 1) fix the typed-`Trip`/`Trip[]` end-to-end crash so typed records are usable at all | low–med | ASan-clean typed-Trip fixture; suite green |
| S1 | interprocedural read-only-non-escaping-arg **summary** + by-borrow ABI for **scalar-field, `Direct`-callee, non-recursive**. ⚠️ **NOT "productionize the H8 prototype" — that prototype is unsound** (see H8 correction): the unsolved core is the **construction-side codegen** (the caller must build the stack record in the *packed scalar layout the borrowed callee reads*, not the boxed/object layout — the current prototype mismatches them and returns garbage). Re-validate the **output value at small N on the construct-in-`.for`-closure shape** (the real hot-loop shape, which the prototype gets WRONG) before any timing. | med→high | **correct VALUE on the in-closure shape first** (gate must provably FIRE on it and produce the right answer); then H8-style microbench win; escaping-negative stays heap; ASan (necessary-not-sufficient — it is blind to the wrong-value bug); full suite green **with the gate proven to fire on the tested shapes** |
| S2 | extend the summary to **recursion** (the `eval`/scan shape) + **indirect/closure callees** (`.for(fn)` — the dominant Lin loop idiom; needs a per-call-site target set) + **cross-module** (serialize the summary into the `.sig`/module cache — the mechanism exists) | med–high | RAPTOR/interp shapes; ASan; `verify`-gate holds; digest byte-identical |
| S3 | drive RC **emission** off the escape/repr fact, not the static type (`lower.rs` `is_rc_type` → "is this value proven stack/borrowed") — delete type-driven emission for proven-stack values (the ADR-062 single-owner direction; the most invasive edit — RC emission is the recurring UAF seam) | high | full ASan harness matrix; corpus run-equivalence; no perf regression |
| S4 | **contiguous value arrays** for value records — subsumes the ADR-063 packed-array effort on the right foundation (no per-element box, no per-read materialize, no per-element-per-field retain — the costs that made the H4 retrofit read-hostile); reads become `base + i*stride + offset` | high | harness {op×position×escape}; interp `Token[]` fast+correct; RAPTOR digest stable |
| S5 | by-value-in-registers ABI for small records (the Go/Zig small-struct ABI; optional final polish) | med | microbench; ABI tests |

**Estimated ceiling:** value semantics spanning the boundary remove ~half of interp's wall time (the
~96%-of-runtime-call alloc+RC portion, the parser being 75% of wall time and all per-step record
allocation). RAPTOR gets ~3.5× from typing alone (Path 1) + ~1.2–1.5× from value semantics on top.

**The highest-value single next step is S2's indirect/closure-callee case** — the H8 prototype only
handles `Direct` callees, but the entire language loops via combinator callbacks (`.for(fn)`), so closing
that is what makes the borrow pay on idiomatic code.

### The complementary lever — inlining / fusion (make the call boundary *disappear*)

The borrow (S1–S2) makes the function-call boundary *cheap*; inlining makes it *vanish* — and the two
are complementary halves of the same "everything is a function" problem. **This is an already-merged,
proven, orthogonal axis** (it is not one of the other four paths — Path 2's "inline" is inline *caches*,
a different mechanism): `range(a,b).for(f)` is fused to a counted loop and capturing literal closures are
inlined at their call site (~2× on a closure-ABI-bound microbench; verified). When a hot loop inlines
fully, there is **no call boundary** — the per-iteration value stays in registers and the *already-live*
escape-based RC-suppression (the inline case, the 0.74 s floor) makes it free, no borrow analysis needed.

So Lin's "everything is a function (even `for`)" cost has two attacks that compose:
- **Inline the call away** (fusion / closure inlining) — already merged, extend its *reach*
  (`arr.for`/`map`/`filter`/`reduce`/`sortBy`; nested combinator chains fused into one loop, the
  Rust-iterator win; recursive tree-walks specialised where the target is static). Risk is CFG/SSA
  correctness at the inline boundary (a body that emits blocks must wire the back-edge to the true
  latch — the known hazard).
- **Borrow across the call you can't inline** (S1–S2) — recursion that doesn't specialise, cross-module
  calls, closures passed as values. These never inline, so the borrow is what removes their RC.

**Together:** inlined loops + borrowed args + stack/value records = the straight-line, values-in-registers
code Go/Rust produce — on Lin's existing static-RC runtime, with no GC and no user-facing borrow checker.
Neither lever alone covers all calls (inlining can't reach non-inlinable calls; the borrow doesn't help a
call that *should* just disappear); pursued together they cover the whole boundary. Inlining is the
lower-risk, ship-incrementally half (no ownership-model change); the borrow is the structural half.

## Acceptance gates

For 3a: each stage above is independently ASan-gated and benchmark-measured; the inferred-borrow
build-out (S1–S5) is a **compiler-internal change with no surface/spec impact** — it applies to the
existing sealed-record type, adds no syntax, and keeps observable behavior unchanged (the only decision
that would touch the spec is a *future, optional* one to expose value-mutation semantics, which is not
required for the performance win). For 3b: escape-analysis soundness — ASan must prove no UAF on region
drop for the inferred variant. Plus the
shared gates: full `cargo test`; harness + IR-mechanism assertion; RAPTOR digest byte-identical;
ASan-clean; cross-language benchmark non-regression (esp. the build-heavy phases — RAPTOR LOAD/PREP,
interp tokenize — where this path should show its win). **And — the lesson of H4 — a perf gate, not only
a soundness gate: a sound change can still regress (the gate-widening passed the soundness harness and
still regressed interp 2.8×).**

## Verdict

The only path that attacks construction RC — the third cost, untouched by the read/dispatch paths and a
large remainder per the spike. **3a (value semantics)** is the deepest "be like Go" answer and fixes all
three costs; it applies to the **existing sealed-record type** (`{ "k": T }`, already distinct from the
`{ String: T }` hashmap) with **no new syntax** — the change is the *behavior* of that type
(reference → value), reached compiler-internally and inferred, not a userland feature. **3b (arenas)** is
the low-risk, semantics-preserving way to claw back construction cost, orthogonal and composable with any
reads-cheap path. Best pursued *composed* — most likely **Path 1 (in-place ABI) + Path 3a/3b** — rather
than alone.
