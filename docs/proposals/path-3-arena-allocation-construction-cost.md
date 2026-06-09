# Path 3 — Kill construction/RC cost without a language change (inferred arenas + escape-based RC elision)

**Status:** Open proposal, one of the paths. Self-contained. **No userland change** — every mechanism
here is a compiler-internal allocation/RC-emission strategy, transparent to program meaning; the surface
language and the reference semantics of records are unchanged.

**Direction in one line:** the read-focused paths (1, 2) cannot touch the *third* cost — per-object heap
allocation + per-field refcounting on construction. Attack that purely inside the compiler: extend escape
analysis so that record graphs with a provably-bounded lifetime are bump-allocated in an inferred region
(no per-object RC, freed in one pass) and so that records that don't escape a call boundary aren't
refcounted across it — the way Go/Rust avoid per-object RC, but **inferred**, with no `region {}` syntax,
no value-vs-reference semantic change, and no borrow-checker surface.

---

## Background (shared context — the problem, the framing, the full history)

### The problem
Reading a field of a known record type, and operating over arrays of such records, is dramatically
slower in Lin than in Go/Rust/Zig/Nim (const-offset loads, const-stride walks there).

### The framing correction
**Lin's type system is not JSON.** The grammar already distinguishes a struct from a map — a fixed-key
`type Person = { "age": UInt8 }` is a sealed struct; `type Counts = { String: UInt8 }` (index signature)
is a hashmap. "Looks like JSON → represented like dynamic JSON" was the root misconception. *This path's
angle:* even once a struct is laid out perfectly, **it is still a refcounted heap object** — the gap to
Go/Rust is partly a **memory-allocation/lifetime** gap (per-object alloc + per-field RC), not only a
layout gap. That cost is invisible to the read/dispatch paths.

### How Lin represents values today
- **Boxed (default, dynamic):** heap `LinObject`, refcounted, string-keyed; `obj["k"]` a non-inlinable
  `lin_object_get`, opaque to LLVM; the representation of `Json`, inferred literals, subtyped params, and
  every value through a polymorphic stdlib op.
- **Packed / "sealed":** const-offset packed struct; array = header-less `0xFE` buffer + per-field RC
  descriptor; scalar packed field read = `getelementptr + load`.
- **Flat scalar arrays:** `Int32[]` already contiguous + specialized.
- Machinery: `Repr` lattice + oracle (ADR-062); gate `is_sealed_array_field_packable` (scalar+Bool only);
  **and existing escape analysis** (ADR-012 closure-cell escape; the inline-record stack-alloc +
  RC-suppression already live on master) — the foundation this path extends.

Crucially: **both** representations are **refcounted heap values.** Even a perfectly packed record pays
per-object heap allocation + per-field retain on construct and a descriptor drop-walk on free.

### The three costs
1. **Field reads through the dynamic ABI** — ~72×; fixable, largely solved by a spike for packed (Path 1).
2. **Operations at the boxing boundary** — dominant for combinator-heavy code: `length(packed Token[])`
   materializes the whole array to boxed `Object[]`; all combinators re-box on entry (Path 1's domain).
3. **Construction / RC** — per-object heap alloc + per-field retain on build + a descriptor drop-walk on
   free, **and** RC paid across every function-call boundary (H8). **This is the cost Path 3 targets and
   the one Paths 1/2 structurally cannot fix.**

### The full history (what was tried, learned, failed)
- **H1 — Profile (valid):** typed vs `Json` field read ~72×; measured already-packed reads.
- **H2 — Leaks drained (independent):** RAPTOR ~190 MB/scan → ~97% reduced; bench completes. *(The RC
  machinery this fought is the same machinery this path would partly elide.)*
- **H3 — Sealed machinery + harness (sound):** per-field RC, descriptors, keep-packed ops, mechanism (i),
  3-point ASan harness (found a `sort` leak manual probing missed).
- **H4 — Gate widenings net-negative:** scalar→String→Array→Map→nested each found+fixed a real bug;
  packing heap fields regressed interp ~3×, crashed TLV, helped RAPTOR nothing; gate narrowed to
  scalar+Bool.
- **H5 — RAPTOR retype: correct, >5× regression**; sub-blockers `get<T,D>` + `Trip|Null`/`Conn` re-boxing.
- **H6 — The cheap-packed-*reads* spike:** making packed heap-field *reads* cheap recovered only ~6% on
  interp; `length`/combinators still materialize the whole array. Reads weren't the bottleneck — **and
  that spike explicitly could not touch construction/RC (#3).**
- **H7 — Ruled out:** boxed inline-slot (unsound); shape-ratio gate (3.6× blind spot); cheap-reads-alone;
  round-key churn (neutral); NaN-box / slab / GC / box-pool (prior *allocator* tweaks — note: arenas are a
  *lifetime* strategy, a different axis from those allocator swaps that failed).
- **H8 — The function-boundary RC cliff (solidly measured) + a borrow spike (UNSOUND — cautionary):** a
  known-type record built+read **inline** is ALREADY free — escape analysis stack-allocs it + suppresses
  its RC (live on master): a 200M-iter inline loop hit **~0.74 s = scalar-floor parity, zero RC,
  alloca→registers**. But passing that same record **to a function** cost **~13×** more — *identical with
  RC-suppression on or off* — because `escape.rs` conservatively treats every call argument as an escape.
  Since Lin is functional (everything, incl. `for`, is a call), the inline win evaporates at the boundary.
  **The cliff itself is solid, load-bearing evidence.** A prototype that tried to *fix* it
  (interprocedural read-only-non-escaping-arg summary → pass-by-borrow, no RC) **was built but is
  UNSOUND** — it returns WRONG VALUES on the construct-in-`.for`-closure hot-loop shape (a caller/callee
  layout mismatch) and its "~1.5×/sound" headline does **not** hold; **no speedup number from that
  prototype should be quoted.** The *problem* (boundary RC is a ~13× cliff, removable in principle) is
  proven; the *solution* is not yet built correctly. **This path takes the lower-risk route to the same
  cost: arenas/region inference, which removes per-object alloc+RC by lifetime rather than by a
  per-argument borrow ABI.**

### The central finding
The packed representation isn't integrated with the verbs (cost #2 — Path 1). **Separately, every value
stays a refcounted heap object allocated per-construction and refcounted across every call (cost #3) —
which no read/dispatch path addresses.** H6 (reads ~6%) proves the remainder is the boundary (#2) +
construction/RC (#3); H8 proves construction/RC at the call boundary is a ~13× cliff. Go/Rust are fast
partly because a record's lifetime is known and it isn't refcounted per-object — and Lin already does
exactly this *inline* (the 0.74 s floor); the gap is making it hold across the call boundary and the
build phase, which is a **lifetime/allocation** problem, solvable in the compiler.

---

## This path's thesis

Records stay reference-semantic, heap, refcounted by default — **no language change.** But for the
common, statically-detectable cases where a record graph's lifetime is bounded, the compiler avoids the
per-object alloc + per-field RC entirely, by extending the escape analysis that already gives the inline
0.74 s floor.

### 3-inferred-arena (the primary, lowest-risk mechanism)
Much construction cost is per-object alloc + per-field RC for graphs with a **bounded, common lifetime**:
RAPTOR's loader builds *all* trips once, the scan reads them, all freed together; interp builds a token
stream, parses it, discards it — textbook build-once / read-many / free-together.

- **Inferred regions, no syntax.** Extend the existing escape analysis: a record graph provably confined
  to a scope (a function body, a loop, the program's build phase) is **bump-allocated in a region** — no
  per-object `malloc`, no per-object refcount — and the whole region is freed in one pass at scope exit.
  Values that escape the region are promoted (copied out / refcounted normally) at the boundary. Lin
  already has closure-cell escape analysis (ADR-012) and inline-record stack-alloc + RC-suppression; this
  is the same analysis generalized from a single inline value to a *graph* and to a region lifetime.
- **No `region {}` keyword.** (An explicit scoped allocator would be a small userland addition; this path
  deliberately stays inferred-only — zero surface change. If a future, separate decision wanted an
  explicit form it could be added, but it is *not* part of this path.)
- **Fixes construction/RC (#3).** The build phase becomes a bump-pointer increment + one bulk free.
- **Does NOT fix reads/boundary (#1/#2) by itself.** A region-allocated *boxed* record still reads via
  `lin_object_get`; arenas are **orthogonal** and compose with Path 1 (which makes reads/ops cheap) or
  with packing (a packed region = contiguous + RC-free).

### 3-escape-RC-elision (the boundary half — built on H8's measured cliff, NOT its unsound prototype)
Generalize the inline RC-suppression across the call boundary: where interprocedural analysis proves a
call argument is read-only and non-escaping, pass it without retain/release. **This is the H8 cliff's
fix** — but the H8 prototype was unsound (wrong values), so this is "build the construction-side codegen
correctly and value-verify," not "productionize the prototype." Lower-priority than inferred arenas
because (a) it is the part that already went wrong once, and (b) much of the boundary cost is *also*
removable by the orthogonal inlining lever (below), which makes the call vanish so no borrow is needed.

### Out of scope (recorded, explicitly NOT proposed — all would be userland changes)
- **Value/copy semantics (the Go-struct model)** would fix all three costs, but it is a **breaking
  semantic change** (reference→value: mutation/aliasing/identity/closure-capture all change). Even done
  additively it requires a user-facing distinction. **Removed from this path** per the no-userland-change
  constraint. *(Note the connection: packed-by-default in Path 1 already captures much of the layout
  benefit value types give, with no semantic change — so the value-semantics idea is largely subsumed by
  "Path 1 packed-by-default + Path 3 inferred arenas," which together approximate "records behave like
  cheap values" without changing their observable semantics.)*
- **Static ownership / borrow checking (Rust)** — language-redefining, out of scope.
- **Explicit `region {}` syntax** — a (small) surface change; excluded to keep this path zero-surface.

### The orthogonal lever — inlining / fusion (already merged; make the call boundary *disappear*)
Independent of allocation, and composable with everything: making Lin's hot **function-call boundaries
vanish**. Lin is functional — *everything is a call*, including `for` and every combinator — so call
overhead is a major cost on its own. Already merged + proven: `range(a,b).for(f)` fused to a counted loop;
capturing closures inlined at literal `.for`/combinator sites (~2× on loop-bound code); flat-scalar
push/set inlined; the boxed-`Object[]` `arr[i].field` fusion. **When a hot loop inlines fully there is no
call boundary** — the per-iteration value stays in registers and the *already-live* inline
RC-suppression makes it free (the 0.74 s floor), with no arena and no borrow needed. **Remaining reach**
(the orphaned follow-on, lower-risk, no model decision): extend closure inlining to
`arr.for`/`map`/`filter`/`reduce`/`sortBy` with literal/capturing closures (latch-relative-CFG back-edge
wiring is the known hazard), nested-combinator fusion (the Rust-iterator win), and indirect/cross-module
callees. This is the cheapest way to remove a large fraction of cost #3's *boundary* component for the
calls that *should just disappear*; inferred arenas handle the build-phase allocation that inlining
can't.

## What this path fixes

- **Construction/RC (#3):** yes — inferred arenas remove per-object alloc+RC for bounded-lifetime graphs;
  escape-RC-elision + inlining remove the per-call-boundary RC. **The only path that targets #3.**
- **Field reads / combinator boundary (#1/#2):** **no, by itself** — compose with Path 1 (packed reads +
  in-place ops) or Path 2 (fast dynamic reads).

## Rationale / why pursue this path

- **It is the only path that addresses construction/RC (#3)** — the cost H6's read spike explicitly could
  not touch, that H8 measured as a ~13× call-boundary cliff, and that dominates the build-heavy halves of
  RAPTOR (loader) and interp (tokenizer).
- **Zero userland change.** Inferred arenas + escape-based RC elision are compiler-internal allocation/RC
  strategies — the surface language, record semantics, and observable behavior are unchanged.
- **Lowest-risk way to claw back construction cost.** It reuses the existing escape analysis and the
  proven inline RC-suppression; it changes *where/how* values are allocated and *whether* RC is emitted,
  not what programs *mean*.
- **Composes with every other path** — it is orthogonal to the read/dispatch decision, so it can proceed
  in parallel with Path 1 or Path 2.

## Cons / risks

- **By itself it fixes only construction**, not reads/boundary — must be composed with Path 1 (or 2) for a
  full win. Path 1 (in-place reads/ops) + Path 3 (inferred arenas) covers all three costs, no userland
  change — the lowest-total-risk "fix everything" combination.
- **Escape-analysis soundness is the risk surface.** A missed escape = a use-after-free when the region
  drops, or a dropped retain across a boundary = a UAF. This is the recurring UAF seam; the inferred
  variant is only as safe as the analysis, and ASan must prove no UAF on region drop / no missing retain.
- **The escape-RC-elision (boundary) half already went wrong once** (the H8 prototype returned wrong
  values). It must be built construction-side-correct and value-verified, not resurrected from the
  prototype — hence arenas are the primary, lower-risk mechanism and the boundary borrow is secondary.
- **Heap fields inside arena'd records** must also be region-owned (bulk-freed) or they leak / are
  double-managed — clean only when the whole graph is in the region.

## Relationship to the other paths

- **Path 1 + Path 3 (inferred arenas)** — the headline composition: Path 1 makes reads/iteration cheap;
  Path 3 makes construction cheap. Together all three costs, **no userland change.** Most likely the
  lowest-total-risk full fix.
- **Path 2 + Path 3** — fast dynamic reads (no new representation) + cheap construction. Also no userland
  change; the "stay dynamic but fast" composite.
- **Path 1's packed-by-default subsumes the layout half of value semantics**, so with Path 3's arenas on
  top, "records behave like cheap values" is approximated *without* changing record semantics — which is
  why the value-semantics option was dropped rather than kept.

## Acceptance gates

Escape-analysis soundness — ASan must prove **no UAF on region drop** (inferred arenas) and **no dropped
retain across an elided boundary** (escape-RC-elision), at multiple Ns. Plus the shared gates: full
`cargo test --workspace`; the sealed harness + IR-mechanism assertion; RAPTOR digest byte-identical
(`group=26203913 range=773022892 journeys=139`); cross-language benchmark non-regression — especially the
**build-heavy phases** (RAPTOR LOAD/PREP, interp tokenize) where this path must show its win.

## Verdict

The only path that attacks construction/RC (#3) — the cost the read paths can't reach and a large
remainder (H6's ~6%, H8's ~13× boundary cliff) — and it does so **entirely inside the compiler, no
userland change**, via inferred arenas (primary, lowest-risk, reuses existing escape analysis) and
escape-based RC elision (the H8-cliff fix, built correctly this time — the prototype was unsound).
Orthogonal and composable with every other path; **Path 1 + Path 3 is the no-language-change combination
that covers all three costs.** Best pursued *in parallel* with a reads path, not alone.

---

## IMPLEMENTATION FINDINGS (2026-06-09)

> **Work reference:** no Path-3 code was written (see below) — but it composes with branch **`path1-packed-records`** (durable handle — `git log path1-packed-records`; worktree `.claude/worktrees/path1-packed` is ephemeral), which landed Path 1's in-place ABI (the read/iteration half) that Path 3's arenas (the construction half) would sit alongside.

Path 3 was in scope alongside Path 0/1 but was **not implemented** — the foundation was verified and the full inferred-arena deferred as out-of-scope-for-this-pass, with two concrete findings.

### The escape-analysis foundation is live and effective (the thing Path 3 would generalize)
Verified the existing escape analysis (`crates/lin-ir/src/escape.rs`, ADR-012 / sealed Stage-4) is wired into the pipeline and works *today*: a non-escaping all-scalar sealed record in a tight 200M-iter loop compiles to ~0 runtime — alloca'd, SROA'd to registers, RC fully suppressed (the historically-documented ~12% stack-alloc regression is gone; emission-side RC-suppression fixed it). This is the live substrate Path 3's inferred arenas would extend from a single inline value to a *graph* + a region lifetime.

### Full inferred arena: NOT attempted (multi-week, highest-soundness-risk; deferred deliberately)
A true inferred arena (bump allocator + region-lifetime tracking on the escape graph + escape-promotion at the boundary + bulk-free of heap-field children) is a multi-week effort whose named hazard is region-drop UAF (a missed escape = use-after-free when the region drops). Per the "soundness is the gate" discipline, a half-built version that risks UAF was not landed. Characterized as deferred with the foundation confirmed.

### The boundary-RC half (escape-RC-elision / borrow-ABI) is confirmed UNSOUND as prototyped — do NOT resurrect it
The H8 read-only-arg borrow prototype (cited elsewhere) returns **wrong values** on the construct-in-`.for`-closure shape — a caller/callee layout mismatch (caller builds the stack record boxed, borrowed callee reads it packed). Independently reproduced: a trivial N=5 program prints garbage (`-1491889465` etc.) instead of `30`; the green test suite proved nothing because the borrow gate fired 0× across all integration tests and ASan is blind to a wrong-*value* read. **The boundary borrow is therefore secondary to inferred arenas** (which this path already states), and if pursued must build the construction-side codegen correctly and value-verify — not productionize the prototype.

### How this composes with what DID land
Path 1 Steps 1+2 (in-place packed iteration, 4.5× — see path-1 findings) fix cost #2 but **not** construction RC (#3). For the build-heavy phases (RAPTOR LOAD/PREP, interp tokenize) #3 remains untouched, so **inferred arenas remain the open, highest-value next lever for construction cost** — composable with the now-landed in-place ABI. The proposal's "Path 1 + Path 3" headline composition stands: Path 1's half is built (scalar scope); Path 3's half is unbuilt.

---

## ADDITIONAL FINDINGS (2026-06-09, NOT merged)

> **Work reference (durable handles — the worktree branches are ephemeral; cite the commits):** B2 RC
> elision `a7366c4` · region allocator mechanism `1e78e9c` · inline-`.for` leak fix `6c1d4e0`/`22c9db1`.
> `git log <hash>` to recover.

A separate overnight pass built **two** pieces the findings above list as unbuilt: a first sound bite of
escape-based RC elision (the "B2" suppression), and the bump-allocator mechanism the inferred arena would
sit on. **The arena *inference* is still deliberately unbuilt — the region-drop-UAF seam — which agrees
with the "NOT attempted" verdict above.**

### B2 — escape-based RC elision for non-escaping heap-field sealed records: BUILT + sound + ASan-proven (`a7366c4`)
The findings above note escape-driven RC suppression already works for *all-scalar* non-escaping records
(alloca + register + RC suppressed). This pass extended it one strict step to a class that section leaves
open: a sealed record with a **heap field** (e.g. `{ name: String, x: Int32 }`) **cannot** stack-alloc
(its drop must release the heap field), but if built locally and provably non-escaping, the owning model's
per-*read* `Retain`s on it are redundant churn `rc_elide` cannot remove (its one-to-one pairing bails when
N>1 reads bunch their scope-exit `Release`s at function exit). B2 keeps the value on the heap, drops all
read-`Retain`s, and keeps exactly **one** `Release` (the construction owner's drop). Strict tri-condition
gate, failing toward *not* suppressing (the UAF seam): (1) class does not escape (a call may only borrow);
(2) **all** the class's `Release`s live in one basic block (no branchy drop); (3) `#Release == #Retain + 1`
exactly. Any failure keeps 100% of RC ops — byte-identical to today's codegen.

**Independently re-verified (not self-reported):** the sealed ASan harness was run with B2 on, then
`escape.rs` reverted to the base commit and re-run — **byte-identical** (same 10 pre-existing
`sort`/`tail_thread` FAILs, same exact leak counts; 30 PASS). So **B2 introduced zero new leaks and zero
UAF/double-free**; the harness FAILs are pre-existing leaks unrelated to this work (they fail on *all*
shape families including `scalar`, which B2 never touches). Fixture: 4 retains + 5 releases → 0 + 1, value
correct; ASan multi-N (200/2k/20k) constant 35 B residual. **This is the genuinely shippable Path-3 piece**
— the class `rc_elide` and the all-scalar stack path both leave behind.

### S3b — bump-pointer region allocator MECHANISM: BUILT, dead/gated (`1e78e9c`)
`crates/lin-runtime/src/region.rs`: `lin_region_push/alloc/pop`, chunked, nested, heap fallback; 3 unit
tests, ASan-clean. **No compiler caller → dead in a normal build, zero behaviour change.** This is the
substrate the inferred arena would call; the inference itself (region-confined-graph detection +
promote-on-escape) was **not** wired, for exactly the region-drop-UAF reason the verdict above gives. The
real prerequisite that section identifies (suppress per-iteration `Retain/Release` *emission* for
region/stack-resident values) is partially discharged by B2 for the non-escaping heap-field class.

### Cross-references
- **path-2 (inline caches):** its diagnosis found an owning `lin_tagged_clone` on every dynamic field read
  is a dominant per-read cost — construction/RC churn in this path's domain, and a candidate for the same
  escape-driven elision B2 applies (drop the clone where the read result is consumed borrowed).
- **path-0 (`Trip|Null` leak `f0d02bf`):** same branch; a discrete RC/codegen leak fix complementary to B2.
- **The H8 borrow prototype:** this pass re-confirmed it UNSOUND (wrong values, construct-in-`.for`-closure
  shape) — matching "do NOT resurrect it" above. **B2 is the *sound* alternative bite of the same cost:**
  it elides redundant RC on a heap-resident value instead of stack/borrowing it, so there is no
  caller/callee layout mismatch to get wrong.

### Bonus fix (orthogonal, real) — commits `6c1d4e0`/`22c9db1`
The inline `range(0,N).for(n => …)` path leaked ~16 B/iter when the callback body boxed the `Json`/union
element param for a dynamic op (`lin_box_int32` shell never reclaimed; 303,634 B at N=20k). Fixed by
reclaiming the scalar→union param-bind box across all inline combinators (`6c1d4e0`, `22c9db1`): leak →
18 B constant, ASan-clean across for/map/filter/reduce/range-for + a push-transfer attack, suite green,
RAPTOR digest identical. Pre-existing and orthogonal to the arena work, but a genuine construction-cost
leak worth landing.
