# Path 4 — Whole-program region inference for escaping build-once data

**Status:** Open proposal, written *after* the Path 0/2/3 build measured the real cost. Self-contained.
**No userland language change.**

**Direction in one line:** the measured bottleneck in both RAPTOR and interp is the per-object
allocation + per-field refcounting of record graphs that are **built once and live for the whole
program/build phase** — not field reads, not the boxing boundary, and not anything a *frame-scoped*
arena can reach (those records escape their frame). Attack it with **whole-program / build-phase region
inference**: detect graphs whose lifetime is the program (or a long build phase), bump-allocate them in
a region with refcounting *suppressed inside the region*, and free the region in one pass at the end.

---

## Why this path exists (and why the previous four were aimed wrong)

The earlier proposals (Paths 0–3) were all written against a **cost model that the Path-0 measurement
falsified**. They assumed RAPTOR was slow because its hot field reads were dynamic `Json` ("type the
trips, get the ~70× read win"). The decisive measurement (below) showed that is **false for RAPTOR**:
typing the trips off `Json` made it **2× slower**, with field-read counts essentially unchanged. So the
representation/read-strategy axis those paths argue over (packed records vs inline caches) is **not where
RAPTOR's cost lives.** This path is anchored on the *measured* dominant cost instead of the assumed one.

This is not "the previous work was wasted" — Path 0 existed precisely to run this measurement before
committing to a read strategy, and it did its job (it returned "reads aren't the bottleneck, don't build
the expensive read ABI"), and it surfaced two real, universal RC-soundness double-free bugs along the
way. But the *direction* those proposals pointed (make reads fast) is now measured to be the wrong lever
for the headline benchmarks. Path 4 redirects onto the cost the data actually shows.

---

## Background (shared context — the problem, the framing, the full measured history)

### The problem
RAPTOR is ~25–30× slower than Node/Go on its query phase; interp is ~10–25× slower than Node/Go. The
instinct that drove the investigation — "a language shouldn't be slow reading a property of a known
type" — is correct *in general*, but the measurement shows it is **not** the dominant cost in these two
programs.

### The framing (now grounded in the grammar)
Lin's type system is not JSON. The grammar already distinguishes: a fixed-key `type Trip = {"tripId":
String, ...}` is a struct; `{ String: Trip[] }` is a map; `Json` is the dynamic escape hatch. Lin
*already* lays out fixed-key records as packed const-offset structs (sealed records) — so the
"field read is a const-offset load" property the framing asks for **already holds for concrete records.**
That is exactly why making reads faster (Paths 1/2) barely moved the benchmarks.

### How Lin represents values today
- **Boxed (dynamic):** heap `LinObject`, refcounted, string-keyed — for `Json`, anonymous/inferred
  literals, and values through the polymorphic stdlib ABI.
- **Packed / sealed:** fixed-key `type T` → const-offset packed struct; scalar field read = `getelementptr
  + load` (already shipped and fast).
- **Inline / stack:** an **all-scalar, non-escaping** record built in an inlined loop body is already
  stack-allocated with RC suppressed (the ~29 ns/iter "inline floor", verified on master — register-
  resident, zero heap alloc).
- The gap: every record that is **NOT** all-scalar-non-escaping is a refcounted heap object — per-object
  `alloc` on construct, per-field retain, a descriptor drop-walk on free, and RC across every boundary.

### The three costs (and which the data implicates)
1. **Field reads through the dynamic ABI** — assumed dominant; **measured NOT dominant** (Stage 0:
   typing RAPTOR trips left `lin_object_get` at 247→250 and made it 2× slower; Stage 2 inline caches:
   interp neutral, RAPTOR −5–6% only).
2. **The boxing boundary** between a typed/packed representation and the dynamic one — real, and it is
   what made the typed retype *regress* (every `tripsByRoute[i]` materializes, every store widens back to
   `Json`); but it is a cost a *second* representation *introduces*, so the fix is to not introduce one.
3. **Construction + refcounting of escaping, long-lived record graphs** — **the measured dominant cost**
   that none of the read paths touch and that frame-scoped arenas cannot reach. **This is Path 4's
   target.**

### The full measured history (what was tried, learned, failed — now with numbers)
- **H1 — Decisive profile (the original, now-recontextualized motivation):** typed-record vs `Json`
  field read ~72× in a microbench. Real, but it measured reads of an *already-packed, in-register* value
  in a tight loop — it did **not** measure a real program's mix, which is why it overstated reads.
- **H2 — Leak drain (independent win, shipped to master):** RAPTOR leaked ~190 MB/scan; a class of
  RC/ownership bugs fixed → ~97% reduction, RSS 6 GB→2.2 GB, bench completes.
- **H3 — Sealed packed-array machinery + 3-point ASan harness (built, sound, but see H4).**
- **H4 — Gate widenings net-negative:** packing heap-field record arrays regressed interp ~3×, crashed
  the TLV codec, helped RAPTOR nothing; gate narrowed back to scalar+Bool. The first hard signal that
  representation/layout was not the lever.
- **H5 — RAPTOR retype: >5× regression** (first attempt) — assumed to be incidental.
- **H6 — Cheap-packed-reads spike:** making packed heap-field *reads* cheap recovered **only ~6%** of
  interp's regression. Second hard signal: reads are not the bottleneck.
- **H7 — Ruled out:** boxed inline-slot (unsound); shape-ratio gate (3.6× blind spot); NaN-box / slab /
  GC / box-pool (prior allocator-swap negatives).
- **H8 — The function-boundary RC cliff (measured) + a borrow-fix prototype (UNSOUND):** a known record
  built+read inline is free (~0.74 s / 200M iters, scalar floor); passing it to a function cost ~13×
  more because `escape.rs` treats every call-arg as an escape. The cliff is real; the borrow *fix* for
  it returned wrong values (caller/callee layout mismatch) and is not trusted.
- **H9 — THE DECISIVE MEASUREMENT (Path 0, the one that grounds this path):** typing RAPTOR's trips off
  `Json` to `{ String: Trip[] }`, ASan-clean, digest byte-identical, measured release build, load <12:

  | Phase (ms) | baseline (`Json` trips) | typed (`Trip[]`) | Δ |
  |---|---|---|---|
  | LOAD | 7156 | 4468 | **−38%** (typed loader builds *fewer transient boxes* — a construction win!) |
  | PREP | 8203 | 8021 | ~flat |
  | GROUP | 24499 (±1.6%) | 50321 | **+2.0× SLOWER** |
  | RANGE | 71832 (±1.5%) | 128549 | **+1.8× SLOWER** |

  Scan IR mechanism: `lin_object_get` 247→**250** (reads did not drop), `sealed_array_to_tagged` 0→0;
  the typed array instead *adds* `lin_array_get_tagged` element materialization and widens the trip back
  to `Json` at every store boundary. **Conclusion: reads were never RAPTOR's bottleneck; the boxing
  boundary is.** Note the LOAD −38% — typing the *loader's* construction (fewer transient boxes) was a
  real win even though the scan regressed. That is the tell: **construction cost is where the lever is.**
- **H10 — Inline caches (Path 2, built, sound, modest):** interp neutral, RAPTOR GROUP −6.5% / RANGE
  −5%. Modest *because Lin already seals concrete records* — the IC only reaches genuinely-`Json` reads,
  which are boxing/arith-dominated, not read-dominated. Confirms H9.
- **H11 — Construction-cost census (Path 3, the finding that defines Path 4):** the dominant construction
  cost in both benchmarks is records that **escape their construction frame and live for the whole
  program**:
  - **interp:** `Token {kind:String, text:String}` built per token, `push`-ed into the token stream that
    outlives the tokenizer and is consumed by the parser. Escapes; has heap fields.
  - **RAPTOR:** `Trip`/`StopTime`/`Transfer` built once in the loader, persist in
    `tripsByRoute`/`transfers` for the entire scan. Escape; heap fields; program lifetime.
  Frame/scope arenas **cannot fire** on these (they're not frame-confined); the records a frame arena
  *could* take are *already* stack-allocated by the merged all-scalar mechanism. **The reachable win is
  whole-program / build-phase region inference** — which is this path.

### ⚠️ CORRECTION 2026-06-09 — H9 conflated two levers; one of them is the biggest shipped RAPTOR win
H9 ("typing RAPTOR's trips made GROUP/RANGE 2× slower, `lin_object_get` unchanged") is correct for the
lever it pulled — `Json`-RECORD → packed struct (`Trip`/`StopTime` field reads). But this path
generalises it to *"making reads fast / de-`Json`-ing barely moved the benchmarks"*, and that
over-reaches, because **"de-`Json`-ing" is two different levers** and the *other* one is the single
biggest RAPTOR speedup on master:

- **`Json`-DICTIONARY → typed `{ String: T }` map** (`routeStopIndex`, `bestArrivals`, `kConnections`):
  changes `m[k]` from `lin_object_get` (tag-check + intern-compare + **box result to `TaggedVal`** +
  optimiser barrier) to a lean `lin_map_get`. `8859f713` measured **PREP 144 s → 25.7 s (~5.6×)**;
  `8ee79a8d` + `3c4ed0b8`/`ea1569c2` typed the rest. **All on master, digest byte-identical.** This is
  exactly an `lin_object_get`-reduction win — the very thing H9 reported as "reads aren't the
  bottleneck." The reconciliation: H9 counted `lin_object_get` on the *record* reads (which typing to a
  packed-but-unpackable `Trip` does not remove) and missed that the *dictionary* `lin_object_get`s were
  separately, cheaply removable by a different type change.
- H9's own table shows **LOAD −38%** from typing the loader's construction and flags it as "the tell" —
  but the path then scopes LOAD/PREP out ("one-time setup") and optimises GROUP/RANGE. PREP is where the
  5.6× landed.

**Implication for this path:** Path 4's *target* (construction/RC of escaping build-once graphs) is a
real and dominant cost in the GROUP/RANGE query phase — that part stands. But the strategic claim that
"the read/de-`Json` axis is not where RAPTOR's cost lives" is only true for the *record-packing* arm; the
*dictionary-typing* arm was never run as an isolated experiment and, where it was run incidentally, won
~5.6×. **Before committing to whole-program region inference, run a per-call-site-CLASS profile of the
query phase** (split `lin_object_get` into dict-lookup-on-`Json` vs record-field-read vs small-map,
attributed to source lines) to see how much remaining query-phase cost is *still* cheap dictionary
typing vs the genuinely-hard construction/RC this path targets. See path-0's RETROSPECTIVE 2026-06-09 for
the full root-cause of why the profiling missed this.

### The central finding (measured, not assumed) — *modulo the correction above*
RAPTOR and interp are slow because they **allocate and refcount large graphs of small records that are
built once and live for the whole program** — and Lin pays per-object `malloc` + per-field retain + a
drop-walk for every one of them, plus RC across every call boundary. Reads are already fast (concrete
records are const-offset); the read paths therefore can't help. The cost is **allocation + lifetime
management of escaping build-once data** — a memory-model/lifetime problem, addressable in the compiler
without changing the language. *(Caveat: "reads are already fast" holds for packed concrete records and
for typed maps; it does NOT cover `Json` dictionaries still on the `lin_object_get`+box path — those are
the cheap dictionary-typing wins above, orthogonal to this path's construction-RC target.)*

---

## This path's thesis

Records stay reference-semantic, heap, refcounted **by default** — no language change, no new syntax, no
observable-semantics change. But the compiler detects record graphs whose lifetime is the **whole
program or a long build phase**, allocates them in a **region** (bump allocator), **suppresses
refcounting inside the region**, and frees the whole region in one pass at program/phase end. This is
the H11 conclusion turned into a mechanism: the escaping-but-program-lifetime records (interp Tokens,
RAPTOR trips) get region allocation that frame-scoped arenas (old Path 3) provably could not reach.

### Why this is the *measured* right lever
- It targets cost #3 (construction + RC), which H9/H10/H11 all implicate, and which **no read path
  touches.**
- The H9 LOAD −38% result is direct evidence: reducing the loader's *construction* (transient boxes) was
  the one thing that got faster in the typed retype. Region allocation attacks that systematically.
- It does **not** introduce a second representation, so it adds **no boxing boundary** (the cost that
  made the typed retype regress). Region-allocated records are the *same* `LinObject`/sealed structs,
  just allocated and freed differently.

### The mechanism (staged, each ASan-gated)
- **4a — Whole-program / build-phase region detection.** Extend the existing escape analysis from
  "non-escaping within a frame" to "confined to a region with lifetime L," where L can be the program or
  an identified build phase (e.g. "everything reachable from `tripsByRoute` after `loadGTFS` returns,
  until program end"). The analysis must prove the *whole graph* (record + heap fields + nested records)
  shares the region lifetime and nothing in it is freed earlier.
- **4b — Region allocator + RC suppression inside.** Records allocated in a region are bump-allocated
  (no per-object `malloc`) and carry no live refcount *within* the region — intra-region references are
  not retained/released (they can't dangle; the region outlives them all). This kills both the per-object
  alloc and the per-field retain for the dominant graphs.
- **4c — Region teardown drop-walk.** At region end, a single descriptor-driven pass releases any
  references *out* of the region (region→heap edges) and frees the region's backing storage in one
  `munmap`/bulk-free. No per-object free.
- **4d — Promotion at escape boundaries.** A reference that genuinely escapes the region's lifetime
  (returned past it, sent across a thread, stored in a longer-lived structure) is copied out / given a
  normal refcount at the boundary. Missing one is a UAF on region drop — the hard soundness gate.

### Out of scope (recorded — would be userland changes or already-failed)
- **Explicit `region {}` syntax** — a surface change; this path is *inferred*-only.
- **Value/copy semantics, a `struct` keyword, borrow checking** — userland changes, excluded.
- **Frame/scope arenas (old Path 3's primary lever)** — measured (H11) to not fire on the dominant
  records; subsumed by 4a's *region*-lifetime (not frame-lifetime) analysis.
- **The H8 read-only-arg borrow** — proven unsound; not resurrected here (region allocation attacks the
  same RC cost by lifetime, which is the lower-risk route).

## What this path fixes

- **Construction + RC of escaping build-once graphs (#3):** yes — the measured dominant cost. interp's
  per-token alloc/RC and RAPTOR's loader+persistent-trip alloc/RC.
- **Field reads (#1):** already fast (concrete records are const-offset); not this path's job.
- **Boxing boundary (#2):** not introduced (no second representation) and not removed (it's small — H9
  shows it's the *retype* that adds it; staying single-representation avoids it). If a residual boundary
  matters, Path 2's inline caches compose.

## Rationale / why pursue this path

- **It is the only proposal anchored on the measured dominant cost** (H9/H10/H11), not the assumed one.
  Every other path optimizes reads, which the data shows are already fast.
- **Zero userland change** — region inference is a compiler-internal allocation/lifetime strategy;
  programs mean exactly what they meant before.
- **It reuses the existing escape-analysis + descriptor + drop-walk machinery** — 4a generalizes the
  escape lattice; 4c reuses the per-field descriptor walk; the sealed/`LinObject` layouts are unchanged.
- **The build-once/read-many shape is exactly what region allocation is for** — and RAPTOR's loader +
  interp's tokenizer are textbook instances. The H9 LOAD −38% is a preview of the win.

## Cons / risks

- **It is a non-trivial new analysis.** Whole-program / build-phase region lifetime inference is larger
  than frame-scoped escape analysis (which is why old Path 3 stopped at the frame boundary). It needs a
  notion of "region lifetime L" and a proof that a whole graph is L-confined — interprocedural,
  potentially whole-program.
- **Soundness is the entire risk surface.** A missed escape (a reference that outlives the region) = a
  UAF on region teardown. RC-suppression-inside-region must be provably safe (nothing in the region is
  freed early). This is the recurring bug class of this whole effort; ASan (`detect_leaks=0` and `=1`) is
  the mandatory judge, and the analysis must be conservative (fail to *not*-region, never to over-region).
- **Region granularity / partial graphs.** A graph that is *mostly* region-confined but has a few
  escaping members needs promotion (4d) — getting the boundary set exhaustively right is the
  `clone_sealed_array`/boxed-boundary problem again, one level up.
- **It does not help reads or non-build-heavy hot loops** — it is targeted at construction of long-lived
  graphs. Programs dominated by something else won't move.
- **Whole-program analysis vs separate compilation / module cache** — region facts that span modules
  must thread through the `.sig`/module-cache mechanism (the same constraint monomorphization hit).

## Relationship to the other paths

- **Supersedes old Path 3's premise.** Path 3 proposed *frame/scope* arenas; H11 measured that the
  dominant records escape the frame, so frame arenas don't fire. Path 4 is "Path 3 done at the lifetime
  the data actually has" — region = program/build-phase, not frame.
- **Orthogonal to and composable with Path 2 (inline caches).** Path 2 made reads of genuinely-`Json`
  data a bit faster (−5–6% RAPTOR); Path 4 attacks construction/RC (the bigger cost). Path 2 + Path 4 =
  fast dynamic reads + cheap long-lived construction, both no-userland-change.
- **Makes Path 1 (packed second representation) unnecessary for RAPTOR** — H9 proved the second
  representation *adds* the boundary that regresses; Path 4 needs no second representation.
- **Builds on the two RC-soundness fixes** found during Path 0 (the union-match-narrow retain and the
  union-tail-call-arg keep-alive) — those are prerequisites for any typed-record/RC correctness and
  should land regardless.

## Acceptance gates

The shared gates — full `cargo test --workspace` green; sealed harness green; RAPTOR digest
byte-identical (`group=26203913 range=773022892 journeys=139`); cross-language benchmark non-regression —
**plus** the soundness gate that is this path's whole risk surface: ASan-proven **no UAF on region drop
and no dropped retain across a promotion boundary**, at multiple Ns, on the real escaping shapes (interp
Token stream surviving into the parser; RAPTOR trips surviving the loader into the scan). And the
**mechanism+wall-clock** evidence the program now requires: the construction-instruction census
(`lin_object_alloc`/`lin_sealed_alloc`/per-field retain counts) must drop on the region'd graphs, AND
the build-heavy phases (RAPTOR LOAD/PREP, interp tokenize) must measurably speed up by median — a
representational/lifetime change must prove both its mechanism (in IR/profile) and its wall-clock.

## Verdict

The path the measurements actually point at: RAPTOR and interp are slow because they allocate and
refcount large graphs of small, escaping, program-lifetime records — a cost the read paths (1, 2) and
frame arenas (old Path 3) all structurally miss, and that the Path-0 LOAD −38% result previews as the
real lever. Whole-program / build-phase region inference attacks it with **no userland change**, reusing
the existing escape/descriptor machinery, introducing **no boxing boundary**. Its cost is a genuinely
larger (whole-program) analysis and a sharp soundness obligation (UAF on region drop is the risk) — but
it is the first proposal grounded in the measured cost rather than the assumed one. Best pursued as the
primary direction, composing with Path 2's inline caches for the residual dynamic reads.
