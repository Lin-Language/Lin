# Path 7 — The memory model is the bottleneck: a tracing GC foundation (+ opt-in value types on top)

**Status:** ❌ **CLOSED-NEGATIVE (2026-06-09) — RETIRED. Do not build this.** The decisive
allocation-bound measurement was run (`investigate/path7-alloc-bound`) and **no workload or phase is
allocation-bound**: turning the *entire* allocator + RC subsystem into a no-op (`LIN_NO_RC` — the ceiling a
perfect bump-allocator + tracing collector cannot exceed) recovers **~0% wall-clock** on interp (0.394→0.394,
reproducing H12), RAPTOR every phase (LOAD/PREP/GROUP/RANGE all ~1.0× under matched load), and even a
deliberately allocation-heavy synthetic (~4%). **The trap it sprang and survived:** RAPTOR has *textbook
GC-bait retention* — 32.9 GB allocated over 1.8 B allocations, never >1.3 GB live, retention **0.039**, 96%
dying young — and removing alloc+RC *still* does nothing, because the cost is the **work done per
allocation** (631 M linear-scan `lin_object_get` `Json` reads + non-inlined calls), not the allocation or
reclamation. A GC makes `ptr += size` cheap and batches `free`; neither is the bottleneck. **The real
levers are [Path 9](path-9-end-to-end-packed-records.md) (packed records → kills the read cost at source)
and [Path 8](path-8-make-functions-free.md) (devirtualize/inline → kills the call cost); a GC touches
neither.** Revisit ONLY if the RC-driven UAF bug class becomes a recurring *production* problem (a
correctness argument, not a perf one). The measured table is in `investigate/path7-alloc-bound`. The body
below is preserved as the considered-and-measured-negative case.

> ## ⚠️ The falsifying measurement (the GC ceiling test — Path 6's, verified)
> A throwaway build with `lin_rc_retain`/`release`/`free` **and** all `lin_*_alloc` turned into no-ops
> (`LIN_NO_RC`) — i.e. *the entire allocation + refcounting subsystem removed* — ran interp at **0.48 s vs
> the 0.408 s baseline: no speedup, slightly slower.** A bump-arena ceiling was 0.42 s — also no win.
> **A tracing GC's entire value proposition is making allocation cheap and reclamation RC-free — exactly
> what this test already simulated the limit of, and it bought nothing on interp.** So for interp this
> path is falsified: you cannot recover a cost that deleting the whole heap+RC subsystem does not recover.
> This path therefore survives **only** for RAPTOR, **only** if the profile shows RAPTOR (unlike interp)
> is genuinely allocation-bound (its leak history — GBs/scan before the H2 drain — is the one reason to
> suspect it might differ from interp). Absent that, the cost is the **non-inlined calls** (Path 6), which
> a GC does not touch. Do not pursue this path on the general argument below without that RAPTOR-specific
> evidence — the general argument was written before the ceiling test was known and the ceiling test beats
> the argument.

**Direction in one line (the original thesis — now bounded by the caveat above):** every prior path tried
to make *records* faster (layout, reads, packing, arenas) and the measurements kept saying that is not
where the time goes — the *hypothesis here* was that the time goes into
**per-object heap allocation + reference counting of a huge population of small, short-lived,
escaping objects.** That is a *memory-management* problem, and the proven fix — used by every fast
managed runtime (V8, Go, JVM, CLR) and by *none* of them solved with naive refcounting — is a
**generational, bump-allocating tracing garbage collector**. It is invisible to userland (RC↔GC is an

**Direction in one line:** every prior path tried to make *records* faster (layout, reads, packing,
arenas) and the measurements kept saying that is not where the time goes — the time goes into
**per-object heap allocation + reference counting of a huge population of small, short-lived,
escaping objects.** That is a *memory-management* problem, and the proven fix — used by every fast
managed runtime (V8, Go, JVM, CLR) and by *none* of them solved with naive refcounting — is a
**generational, bump-allocating tracing garbage collector**. It is invisible to userland (RC↔GC is an
implementation detail), it fixes a latent correctness bug (RC can't collect cycles), and it is the
*floor* under both fast models Lin could adopt next (Go-style value types; V8-style hidden classes).

---

## Why this path exists — the one finding all of Paths 0–5 converge on

The five prior proposals argued over *read strategy* and *record layout*. Read them as five independent
experiments and they all measure the **same** thing:

| Experiment | Result | What it rules out |
|---|---|---|
| **H6** — make packed reads cheap | recovered ~6% of interp | reads are not the bottleneck |
| **H9** — type RAPTOR's data off `Json` | **regressed 2×** | representation/layout is not the bottleneck |
| **H8** — record used inline vs passed to a fn | **13× more expensive at the call boundary, identical with RC-suppression on OR off** | the cost is heap-alloc + treating the value as escaping, NOT the RC instruction count |
| **H11** — construction-cost census | dominant cost = small objects that **escape their frame and live for the whole program** | frame arenas can't reach it |
| **Path 5** — make records value types | would fix it, but records are **observably mutable references** → breaking | the cheap fix isn't free; it's a semantic change |

These do not point five directions. They point one: **Lin is slow because of its memory model — per-object
heap allocation plus reference counting applied to every aggregate value — not because of how records are
shaped or read.** That is a real, located conclusion, and it is *load-bearing*: it rules out the entire
"make reads/layout faster" family (Paths 1, 2-as-primary, the packing work) as the *primary* lever.

## The uncomfortable framing that makes the fix obvious

**Lin is a natively-compiled language that is slower than CPython.** CPython is the canonical *slow*
baseline: a bytecode interpreter where every value is a refcounted `PyObject`. If an LLVM native-code
compiler loses to it, the explanation is not "compiled languages can't be fast here" — it is that **Lin
adopted CPython's memory model (heap-allocate-and-refcount-everything) without CPython's excuse (being
interpreted).** The four concrete design choices that cost the performance:

1. **Every aggregate is a heap-allocated, refcounted object.** A 2-field `{x:1, y:2}` is a `malloc` + an
   RC header + a descriptor. Go puts that in a register or on the stack for free.
2. **Reference counting is the memory-reclamation strategy.** RC is *known* to lose to a good tracing GC
   on allocation-heavy workloads: you pay retain/release on every escape and every call boundary (the H8
   13× cliff) and you *still* don't get cheap allocation — the worst of both. **No high-performance
   managed runtime uses naive RC** (V8, Go, JVM, CLR are all tracing-GC). That is not a coincidence.
3. **A second, dynamic boxed ABI (`TaggedVal`) is the universal currency** for every polymorphic stdlib
   op, so any faster representation is materialized *back* to boxed at every `length`/`map`/`for` (cost
   #2 — a tax that exists *only because there are two representations*).
4. **Everything is a function call** (Lin is functional; `for`, every combinator), so call + closure
   overhead is pervasive (the orthogonal inlining lever, partly shipped).

**None of these are language-*semantics* decisions.** They are implementation strategy. That is exactly
why the headline fix can be non-breaking.

---

## This path's thesis: replace refcounting with a generational tracing GC

The lever no prior proposal named: **stop reclaiming memory with per-object refcounting; reclaim it with a
generational, bump-allocating tracing collector.**

### Why this is *the* lever (it targets the measured cost directly)
- **Allocation becomes a pointer bump.** A nursery is a contiguous arena; `alloc` is `ptr += size;
  bounds-check`. This is *precisely* why V8/Go are fast at "lots of small short-lived objects" — which is
  exactly RAPTOR's scan and interp's tokenizer. The per-object `malloc` + RC-header cost (design choice #1)
  collapses.
- **Short-lived escaping garbage is collected in batch, not refcounted one object at a time.** H11's
  dominant cost — small objects that escape their frame — stops being per-object retain/release/free and
  becomes "the nursery fills, survivors are copied, the rest is reclaimed in one pass." Generational GC is
  *built for* the build-once / discard-fast shape (the generational hypothesis: most objects die young).
- **The H8 call-boundary cliff largely evaporates.** Passing an object becomes passing a pointer — no
  retain on the way in, no release on the way out. The 13× boundary cost was retain/release + the value
  being treated as escaping; under a GC neither applies (the collector discovers reachability; the
  compiler doesn't have to conservatively retain).

### Why it's non-breaking (the decisive advantage over Path 5)
RC↔GC is an **implementation detail**. Observable semantics are identical: same reference behavior, same
aliasing, same mutation visibility (`val b = a; a["x"]=9; b["x"]==9` still holds — see
`project_records_are_reference_types`). **Nothing in userland changes.** Go, Java, C#, and JS all made
exactly this choice and no one writing those languages thinks about it. This is the critical contrast with
Path 5 (value records), which *is* a breaking semantic change.

### Bonus correctness win
RC cannot collect cycles. Lin today has **no cycle collector** (`Shared<T>` can leak reference cycles,
ADR-024). A tracing GC collects cycles for free — this closes a known soundness/leak gap, not just a perf
gap.

### Why Lin is unusually well-suited to a GC (the part that de-risks it)
The hardest parts of a production GC are concurrency coordination and write barriers. **Lin's design
removes both:**
- **Disjoint per-thread heaps.** ADR-043: values crossing a thread boundary are *deep-copied*, so each
  thread owns a private, disjoint object graph — nothing is shared. That means the GC can be **per-thread**:
  a thread collects its own nursery with a stop-*this-thread*-only pause (sub-millisecond on a small
  nursery), no cross-thread synchronization, no concurrent-marking complexity. This is the single biggest
  reason a GC is tractable here and not a multi-year project.
- **Mostly-immutable-after-construct records** → almost no mutations of old→young pointers → **minimal
  write barriers** (the other classic GC complexity). `var` cells and index-assign are the only mutation
  sites and are already special-cased.
- **The runtime already has precise type descriptors** (the per-field RC descriptors / `elem_desc` from
  the sealed work, the `TaggedVal` tag scheme) — a tracing GC needs exactly this "where are the pointers
  in this object" map, and it is *already built* (it currently drives the RC drop-walk). The GC reuses it
  as its trace map.

### Why this is the more-robust version of Paths 3/4 (arenas/regions)
Paths 3 and 4 were *inferred, compile-time GC*: prove a lifetime, free in one pass. They kept hitting the
soundness wall — proving "this whole graph is region-confined" interprocedurally is hard and a missed
escape is a UAF. **A tracing GC discovers reachability at runtime instead of proving it at compile time.**
It is the same idea (batch-reclaim a generation of objects) made robust by not requiring a proof. Paths 3/4
were groping toward this; Path 6 is the version that actually works.

---

## What sits on top of the GC (the two proven fast models — both *require* a GC underneath)

The GC is the **floor**, not the whole answer. With it in place, the two proven high-performance models
become reachable, and the user's "everything inline on the stack" instinct is *correct* — it's just the
*second* layer, not the first:

### Layer 2a — opt-in value types (the non-breaking Go model)
Path 5 proved records are observably-mutable references, so making *all* records value types is breaking.
But making value semantics **opt-in and additive** is not: a distinct value-record form (a `struct`-like
declaration, or value-semantics on an explicitly-marked sealed record) for the **hot leaf data** (RAPTOR's
`Trip`/`StopTime`, interp's `Token`) — stack/inline, zero per-object cost, value-copy semantics the
programmer opts into — while today's reference records are unchanged. This is the Go model (value structs +
GC'd heap graph) delivered additively. It needs the GC underneath for the heap graph the value types point
into.

### Layer 2b — hidden classes + inline caches (the V8 model — old Path 2)
For genuinely-`Json`/dynamic code, V8-style shape-guarded inline caches make dynamic field reads
const-offset. Path 2 built this and measured it sound; it recovered only ~5-6% *because* the bottleneck was
allocation/RC, not reads. **On top of a GC** (cheap allocation) it speeds up the residual dynamic reads.
Also needs a GC underneath (V8 has both).

The structure: **GC is the foundation; value types and inline caches are the two superstructures.** Every
prior path was trying to build superstructure on the wrong foundation.

---

## Staged plan (each stage de-risks the next; gated on the profile first)

- **Stage 0 — the profile (PREREQUISITE, in flight).** `investigate/raptor-typed-profile`: fully type
  RAPTOR, then attribute GROUP+RANGE cost across {allocation, RC traffic, map lookups, boxing boundary,
  algorithm}. **This proposal proceeds only if allocation+RC+boundary dominate.** Baseline recorded:
  half-typed master = GROUP ~109s / RANGE ~314s, digest `group=26203913 range=773022892 journeys=139`.
- **Stage 1 — instrument allocation.** Add an allocation-rate + live-set + RC-op counter (env-gated, like
  the existing `LIN_COUNT` spike). Measure bytes-allocated-per-scan and RC-ops-per-scan on RAPTOR + interp.
  This quantifies the ceiling: if RAPTOR allocates GBs/scan, a bump allocator's win is large and certain.
- **Stage 2 — a non-moving, mark-sweep collector behind a flag (de-risk the trace machinery).** Reuse the
  existing field descriptors as the trace map. Keep `malloc` allocation at first (don't move objects yet),
  replace RC reclamation with mark-sweep. This proves the trace map is complete (the hard correctness gate)
  *without* the complexity of a moving collector or pointer fix-up. ASan + the full suite + RAPTOR digest
  are the gates. RC can be kept as a fallback (flag-selected) during bring-up.
- **Stage 3 — a bump-allocating generational nursery (the actual perf win).** Per-thread nursery (disjoint
  heaps, ADR-043 → no cross-thread sync), bump allocation, copy survivors to an old generation, minimal
  write barrier on the few mutation sites. This is where the allocation cost collapses. Measure RAPTOR +
  interp + the cross-language suite.
- **Stage 4 — remove RC.** Once the GC is proven, delete the retain/release emission and the per-object RC
  headers (the codegen/lower.rs RC machinery, the descriptor drop-walks). This is also where the recurring
  UAF/double-free bug class (the entire `project_rc_ownership_invariants` saga) *disappears* — there is no
  manual ownership to get wrong.
- **Stage 5 (separate, additive) — opt-in value types** (Layer 2a) and/or **inline caches** (Layer 2b),
  each gated on its own measured win, on top of the GC.

---

## What this fixes vs the prior paths

- **Construction/RC (cost #3, H8, H11):** **yes — directly, this is the point.** The only path that
  attacks it at the root (allocation strategy) rather than by inference (Paths 3/4) or by representation
  (Path 5).
- **The boxing boundary (cost #2):** indirectly eased — cheap allocation makes materializing-to-boxed
  cheap, and the value-type layer (5a) can remove it for hot data. Not dissolved by the GC alone.
- **Field reads (cost #1):** not by the GC; by Layer 2a (value types, const-offset) or 2b (inline caches).
  But H6/H9 already showed reads aren't the bottleneck.
- **Cycle leaks (`Shared<T>`, ADR-024):** **fixed for free.**
- **The UAF/double-free bug class:** **eliminated at Stage 4** (no manual RC ownership).

## Cons / risks (significant — this is the biggest single change proposed)

- **It is a large, foundational change** — a new allocator, a collector, root-set discovery (stack maps for
  precise GC, or a conservative stack scan to start), and removing the RC machinery threaded through
  lower.rs/codegen. Bigger than any single prior path. Mitigated by the staging: Stage 2 (non-moving,
  RC-fallback-flagged) de-risks the trace-map correctness before any moving/perf work.
- **Precise root finding in an LLVM AOT backend** is the classic hard part: you need stack maps (LLVM's
  `gc` statepoints / `@llvm.gcroot`, or a shadow stack) to find roots precisely, OR a conservative stack
  scan (simpler, slightly less precise, can't move objects a conservative root points to — pins them).
  Starting conservative (Boehm-style or a conservative-roots + precise-heap hybrid) is a known de-risking
  path. This is the single biggest unknown and Stage 2 should settle the approach before Stage 3.
- **Latency/pause behavior** must stay acceptable. The per-thread disjoint-heap design (ADR-043) makes
  pauses local and small, but it must be measured (the runtime is used for servers/streams — `std/http`,
  `std/stream`).
- **The deep-copy thread-transfer model interacts with GC.** Transfer currently deep-copies; under a GC the
  copy still produces a disjoint graph in the target thread's heap (fine), but `Shared<T>`/`Frozen<T>`
  (atomic-RC boxes shared across threads) become the *one* place that still needs cross-heap coordination —
  they'd stay refcounted (atomic) as a special case, or become a shared old-gen with its own collector.
  This is a real design corner to nail, not a blocker (it's already the one shared-across-threads case).
- **It does not, by itself, give value-type layout or const-offset reads** — those are Layer 2, additive
  and separately gated.

## Relationship to the other paths

- **Supersedes Paths 3 and 4** as the construction-cost fix: arenas/regions are inferred compile-time GC
  that kept hitting the soundness wall; a tracing GC is the runtime-discovered, robust version of the same
  idea.
- **Makes Path 5 (value types) additive and safe** instead of breaking: value types become an opt-in Layer
  2 on top of the GC, for hot leaf data only, leaving reference records (and their mutation semantics)
  intact.
- **Makes Path 2 (inline caches) pay off**: Path 2 was sound but recovered ~6% because allocation/RC, not
  reads, was the cost. On a GC floor it speeds up the residual dynamic reads.
- **Composes with the orthogonal inlining/fusion lever** (partly shipped) — that removes call boundaries;
  the GC removes allocation cost; independent and additive.

## Acceptance gates

The profile (Stage 0) must justify it. Then per stage: full `cargo test --workspace` green; RAPTOR digest
byte-identical (`group=26203913 range=773022892 journeys=139`); ASan-clean during bring-up (the GC and RC
must agree on reachability — a GC that frees a live object is the new UAF, so the trace-map completeness is
the correctness gate, proven at Stage 2 before any moving); cross-language benchmark non-regression and
then **regression** (this path must show a *measured allocation-cost win* — bytes-allocated-per-scan down,
GROUP/RANGE wall-clock down — or it is not worth the change); pause-time measured on the server/stream
workloads.

## Verdict

**This path's general argument was beaten by a measurement it was written without.** The case below —
"Lin adopted CPython's refcount-everything model; replace RC with a tracing GC" — is *architecturally*
sound and would, in isolation, be the textbook fix for an allocation-bound managed language. But the GC
ceiling test (the ⚠️ caveat at the top) shows interp is **not** allocation/RC-bound: deleting the entire
heap+RC subsystem bought *no* speedup. A GC cannot beat the limit of having no allocator and no RC at all,
so for interp this path is **falsified**, and [Path 6 (non-inlined call/dispatch cost)](path-6-eliminate-call-dispatch-cost.md)
is the measured direction instead.

What survives, narrowly: **RAPTOR is not yet measured on this axis**, and its leak history (GBs/scan
pre-drain) is the one concrete reason to suspect it is allocation-bound in a way interp is not. *If* the
in-flight profile shows RAPTOR's GROUP/RANGE is dominated by allocation + RC traffic (not by the
non-inlined `lin_object_get`/closure/dispatch calls Path 6 targets), then a GC — with its genuine
non-perf wins (invisible to userland unlike value types; fixes the cycle-leak and the UAF bug class;
unusually tractable given disjoint-per-thread heaps + the already-built trace map) — becomes worth its
considerable cost as RAPTOR's foundation, with opt-in value types and inline caches as additive layers on
top. **Absent that RAPTOR-specific evidence, do not build this** — the ceiling test says the cost is the
code (the calls), not the data (the allocation), and Path 6 is where the lever is. This document is kept
as the considered-and-bounded case for a GC, not as a recommendation: the recommendation is Path 6 unless
the profile says RAPTOR is the exception.
