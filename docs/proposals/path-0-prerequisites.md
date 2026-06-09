# Path 0 — Prerequisites: path-independent fixes + the decisive RAPTOR measurement

**Status:** Prerequisite. **Do this FIRST, before committing to Path 1 (packed records) or Path 2
(inline caches).** Self-contained. **No userland language change.**

**Direction in one line:** before choosing a read strategy, land the correctness fixes and the
de-`Json`-ing that *every* path needs anyway, then **measure** — the measurement tells you which of
Path 1 / Path 2 is even necessary, instead of picking blind.

---

## Why this is its own path (not part of Path 1)

These items were previously buried as "Step 0" inside [Path 1](path-1-integrate-packed-records.md),
which made path-independent work look Path-1-specific. It isn't. RAPTOR cannot be fast under **any**
read strategy while its hot data is dynamic `Json` (756M `lin_object_get` scans in the query phase), and
two of the blockers are plain compiler/RC bugs that bite typed-record code regardless of how reads are
ultimately lowered. So these belong *under* the 1-vs-2 decision, not inside one arm of it.

This is also where the framing lands concretely: **the surface distinction already exists.**
`type Trip = { "tripId": String, "stopTimes": StopTime[], ... }` is a closed `Type::Object`;
`tripsByRoute: { String: Trip[] }` is a `Type::Map`; `Json` is the dynamic escape hatch. RAPTOR is slow
because its trips are typed `Json` (a historical workaround), not because the type system can't express
them — typing them off `Json` is the win under every path.

## The work (three universal fixes + a de-`Json` retype)

1. **Fix `get<T,D>` monomorphization for `T = record-array`** (the link error). A generic over a
   record-array value type currently fails to monomorphize. **Universal** — a plain compiler bug; any
   typed-record code calling a generic (RAPTOR's `get(tripsByRoute, routeId, [])`) hits it under any
   path. Also a down-payment on Path 1's Step 2b monomorphization.
2. **Fix the `Trip|Null` tail-recursive scan-param UAF / repr-demotion.** An RC/codegen soundness bug in
   the *existing* model (the scanRouteAt shape). **Universal** — independent of packing or read strategy.
3. **Type RAPTOR's trips off `Json`** — `tripsByRoute: { String: Trip[] }`, and the hot-path params as
   `Trip`/`StopTime`. **Universal** — getting the hot data onto a known type is the prerequisite for fast
   reads under *every* path (Path 1 packs it; Path 2 inline-caches it; either way it must stop being
   `Json`). The packed const-offset read it relies on **is already shipped**.
   - *(One sub-item is Path-1-specific, not universal: the `sort$Object` comparator reading packed
     `Trip[]` elements as boxed only bites if the trips are **packed** (Path 1's representation). Under
     Path 2 there is no packed array to mis-read. Fold that fix into Path 1, not here.)*

## The decisive measurement (the real reason to do this first)

After the fixes + retype, **measure RAPTOR (LOAD/PREP/GROUP/RANGE), digest byte-identical**
(`group=26203913 range=773022892 journeys=139`). The result branches the whole roadmap:

- **If RAPTOR's hot path is dominated by the now-cheap field *reads*** → typing alone wins (~110 s → ~30 s
  projected, ~3.5×), and **none of Path 1's multi-week in-place ABI is needed for RAPTOR**.
- **If `length`/`for`/combinator calls still dominate** (they materialize the packed array — cost #2) →
  it will still regress, which is the concrete proof that **Path 1's in-place ABI (or Path 2's
  no-second-representation) is the actual prerequisite**, and tells you which.

This reconciles the two contradictory historical results — **H5** (a naive retype regressed >5×) vs
**H5b** (the typed-read micro projects ~3.5×) — empirically, cheaply, and *before* spending weeks on
either read strategy. It is the single highest value-to-risk action available and it is strategy-neutral.

## What this fixes / does not fix

- **Fixes:** RAPTOR's `Json` field-read tax (#1) for its hot path; two latent compiler/RC bugs; and it
  produces the measurement that selects Path 1 vs Path 2.
- **Does not fix:** the combinator boxing boundary (#2 — that's Path 1 or Path 2) or construction RC
  (#3 — that's [Path 3](path-3-arena-allocation-construction-cost.md)). interp is barely moved by Path 0
  (its cost is construction, not Json reads — it's already typed).

## Relationship to the other paths

- **Strict prerequisite to Path 1 and Path 2** — both want RAPTOR's trips typed; neither can show its
  RAPTOR win until Path 0 lands. Path 0's measurement is what *chooses* between them.
- **Independent of Path 3** (arenas/construction RC) — Path 3 helps RAPTOR's build-once LOAD/PREP phases
  and interp regardless; it composes with whatever Path 0 + (1|2) decide.
- **No 1-vs-2 commitment** — Path 0 is deliberately strategy-neutral so the decision is made on the
  measured data it produces, not in advance.

## Acceptance gates

Full `cargo test --workspace` green (the `get<T,D>` + `Trip|Null` fixes are correctness/RC — ASan-clean,
no UAF/double-free); RAPTOR digest byte-identical before and after the retype; the per-phase RAPTOR
timing recorded (the deliverable that drives the next decision); cross-language benchmark non-regression.

## Verdict

The unconditional first move. It removes RAPTOR's `Json` tax, fixes two real compiler bugs, requires no
language change and no strategy commitment, and — most importantly — its measurement is what tells you
whether you need Path 1's expensive ABI at all and whether Path 1 or Path 2 is the right read strategy.
**Do Path 0, measure, then choose `{1 or 2}` + `3`.**

---

## IMPLEMENTATION FINDINGS (2026-06-09, branch `path1-packed-records`, NOT merged)

> **Work reference:** branch **`path1-packed-records`** (durable handle — `git log path1-packed-records`; worktree `.claude/worktrees/path1-packed` is ephemeral). The Path-0 deliverable is commit **`04bec701`** (the `Trip|Null` tail-recursive UAF fix — merge-worthy on its own). Full commit chain + the Path-1 work that builds on it is in path-1's IMPLEMENTATION FINDINGS section.

Worked through Path 0 in full. Outcome: **the two bug fixes are real and one landed sound; the decisive RAPTOR measurement was taken and it settles the H5-vs-H5b contradiction empirically.**

### Fix 1 — `get<T,D>` monomorphization for record-array `T`: NOT a bug on current master
Could not reproduce the proposal's "link error" (`undefined reference to std_array_get__val`) on any minimal cross-module repro or on the real RAPTOR retype (`tripsByRoute: { String: Trip[] }` builds end-to-end). The underlying cross-module generic-capturing-closure checker bug had **already been fixed and merged** before this work. So this prerequisite is **already satisfied** — strike it from the path.

### Fix 2 — `Trip|Null` tail-recursive scan-param UAF: REAL, FIXED, ASan-verified, committed (`04bec70`)
A genuine universal RC/codegen soundness bug (the `scanRouteAt` shape). Root cause in `lower.rs`: a concrete heap record (`match`-narrowed `Trip`) re-boxed into a `T|Null` union for a self-tail-call arg becomes the next iteration's param-slot value, but `box_value`(Object→union) wraps a *borrowed* inner; `release_owned_for_tail_call` then frees the narrowed source on the live back-edge → the threaded box dangles. Fix: `Retain` the caller-owned-shell box arg so the +1 transfers into the threaded box. **Verified by me independently:** ASan-clean (no UAF/double-free), correct output (1999 / 199999 across the shape), leak **constant** 88 B / 8 allocs at N=1k AND N=100k (no per-iteration scaling). Regression test added. This fix is **merge-worthy on its own**, independent of the perf roadmap.

### The decisive measurement — RAPTOR is in the COMBINATOR-DOMINATED branch
Typed `tripsByRoute: { String: Trip[] }`, full `bench.lin`, digest byte-identical (`group=26203913 range=773022892 journeys=139`). Per-phase timing was **perf-NEUTRAL** vs the `Json` baseline (LOAD/PREP/GROUP/RANGE all within noise). 

**The branch is decided: typing alone does NOT win RAPTOR — Path 1's in-place ABI IS the prerequisite.** Concrete reason: `Trip` has String + nested-record fields, so under the current gate (scalar+Bool only) it **cannot pack** — it stays a boxed `Object[]`, and the query phase (RANGE ~300s + GROUP ~97s of the ~360s) is dominated by the combinators (`for`/`length`/scan) reading it, not by isolated field reads. This **empirically reconciles H5 (>5× regression on naive retype) with H5b (~3.5× read-micro projection):** typing wins only when the value can *pack* AND the combinators read it *in place* — neither held for `Trip` until Path 1 Steps 1+2 land (see path-1 findings) and the heap-field gate widens (still blocked — see path-1 + path-3 findings).

**Net Path-0 verdict:** prerequisite #1 already done; #2 fixed soundly; the measurement says RAPTOR needs Path 1's in-place ABI (now partially built) before typing trips pays off — and even then only once heap-field records can pack (the open repr-oracle blocker).
