# Performance take five — closing the call/value axis on RAPTOR

> Engineering plan, not marketing. Read `docs/PERFORMANCE.md` §5.6–§5.8 first — this
> document is the *next step* after that record, and assumes its conclusion.

## 0. Where this starts

The campaign in `PERFORMANCE.md` ended on a definitive, measured conclusion (§5.8):
**the data/representation layer is exhausted.** Removing every per-iteration data cost on
RAPTOR — materialization, RC, allocation, literals, hashing, map-value boxing — *and*
copying Go's data design were all **wall-neutral** (~82–91 s, all digest-exact
`group=26203913 range=773022892 journeys=139`). The rdtsc cycle profile says why: `lin_map_get`
9.5 %, field lookups 3.7 %, alloc/box-unbox/ptr-chase/`tagged_eq` ~2 %, typed arithmetic 0 %
— **≤ ~16 % total.** The other **~85 % is the call/value axis**: closure + loop dispatch,
control flow, and the per-iteration safe-access null/bounds/union checks Go does not pay.

The 2026-06-22 investigation (PERFORMANCE.md §5.8 update) corrected the *framing* of the lever:

- **The combinator loop-flattening is already done.** The RAPTOR hot loop
  `range(a,b).for(pi => …)` — a *capturing* literal lambda over mutable `var`s, outer `val`s,
  and params — already inlines its whole body into a flat native loop
  (`for_header`/`for_body`/`for_latch`, no closure alloc, no per-element indirect dispatch),
  verified by reading the emitted IR. The per-stop helpers (`previousArrival`/`bestArrival`/
  `setTrip`/`getTrip`) are **direct monomorphic calls**, not indirect dispatch.
- **All `.lin` files compile into one LLVM module.** `compile_import_from_ir` reuses the same
  `Codegen`/`self.module` (`crates/lin-compile/src/lib.rs:333-394`; `crates/lin-codegen/src/codegen/mod.rs:236-303`),
  optimized once with the standard `default<O2>` pipeline (`mod.rs:353`), emitted as one `.o`.
  So **cross-file inlining is not a boundary** — it is intra-module inlining. This is also why
  "LTO showed no speedup": for user code there is nothing to LTO *across*; it is already one module.

So the loop is structurally flat and the calls between user functions are already direct and
freely inlinable. What LLVM still cannot fold is the **work underneath**: the opaque runtime
calls (Point 1), the residual indirect callbacks (Point 2), and (mildly) external linkage
(Point 3). A fourth boundary is the **value ABI** — records/unions materialized across function
*returns* (Point 4) — and one **cross-cutting** lever, profile-guided block layout (Point 5),
rides on top of all of them. The thesis: **none of the call boundaries is
user-function-to-user-function; the boundary that matters is user-code-to-runtime, and that is
where the ~85 % lives.** The plan is organized so these attack tracks run in parallel — see
*Sequencing plan*.

The honest prior, set by the whole campaign: every *piece* of this tried in isolation came back
small (bitcode runtime <2 %, Perceus reuse not worth it, sealed stack-alloc negative). The bet
here is **synergy** — a real induction variable (from range-for fusion, already in flight) makes
an inlined bounds check provably dead; inlined RC pairs cancel; internal linkage lets the inliner
delete what it absorbs. None works alone. Each step is gated on the RAPTOR digest + wall time, and
may still land wall-neutral — in which case that is one more direct probe of the call/value ceiling.

---

## Point 1 — the runtime static library is opaque

### The problem (verified)

Every hot runtime operation is a **declaration with no body** in the LLVM module — its code lives
in the separately-compiled static archive `liblin_runtime.a` (`lib.rs:1764`). Samples from
`crates/lin-codegen/src/codegen/runtime.rs`: `lin_map_get` (:240), `lin_rc_retain` (:227),
`lin_box_int32` (:206), `lin_sealed_alloc`/`lin_sealed_release` (:232-233), plus
`lin_array_get_tagged`, `lin_unbox_int32`, and the flat-array accessor `lin_flat_array_get_i32`
(whose `_oob` suffix means the **bounds check lives inside the opaque call**). The O2 inliner
cannot see through any of these. In the hot loop the per-iteration work *is* these calls:

- a string-keyed `lin_map_get` (the 9.5 % bucket),
- `lin_rc_retain`/`lin_sealed_release` pairs around borrowed reads,
- `lin_box_int32`/`lin_unbox_int32` round-trips at union/param boundaries,
- the bounds/null/union safe-access checks, sealed inside `_oob` accessors or `?? default` guards.

Because the runtime is opaque, a box never meets its unbox, an RC retain never meets its release,
and a bounds check is never proved redundant — even when the surrounding loop makes all three
trivially foldable. This is the dominant boundary and the real lever.

Codegen *can* emit these inline — it already does in places: the byte accessor emits an inline
`icmp`+conditional-branch+GEP bounds check (`mod.rs:1263-1276`, `sba_oob`), and flat scalar reads
inline to GEP+load in some contexts (the merged "flat-array read inlining", ~3.6× dijkstra). The
machinery exists; it simply does not fire on the RAPTOR hot path, which fell back to the opaque
`lin_flat_array_get_i32` call.

### Suggested fixes (surgical → blanket)

**1(a) — Compiler-side bounds/null-check elision + `_unchecked` variants. [highest EV — new]**
When the compiler can *prove* an index in-bounds, emit an inline in-bounds GEP + load (or a
`lin_flat_array_get_*_unchecked` with no OOB branch); likewise drop `?? default` null guards on
provably-non-null values, and `is T` union tags on provably-monomorphic values. **This is where the
range-for fusion pays off**: once the loop index `pi` is a real `i32` induction variable with bound
`routePathLength`, and `routePathLength == routePath.length()`, then `routePath[pi]` is provably
in-bounds and the check deletes. The opaque `_oob` call is exactly what blocks LLVM from doing this
itself (it cannot do scalar-evolution across an external call), so it must be a Lin-side pass over
the flat IR. Implementation: a small range/bounds analysis on the lowered IR (the loop already
carries the `range` bounds after fusion) that rewrites in-bounds `Index` instructions to an
unchecked form, and a matching codegen path that emits the inline GEP. **range-for fusion + this is
a genuinely untried combination** — the campaign never had a real IV to elide against.

**1(a-i) — prove it with LLVM before hand-rolling it. [test-first]** The Lin-side pass is only
needed for what LLVM cannot do itself. The reason it currently can't is the *opaque* `_oob`
accessor — LLVM cannot run scalar-evolution across an external call. So the cheapest first move is
to **de-opacify the accessor** (emit the flat-array read as an inline `inbounds` GEP + a single
*dominating* `icmp`/branch, extending `sba_oob`, `mod.rs:1263`) and **enable LLVM's Inductive
Range Check Elimination** (it is experimental and *not* in the default pipeline) plus extra
LICM/LoopUnswitch rounds. With step E0.1's induction variable now visible in-IR, LLVM's SCEV may
delete the dominated checks itself — partially subsuming this pass. Emit `!nonnull`/`!range`
metadata and `inbounds` GEPs to help it; use `llvm.assume` only sparingly (it can become an
optimization barrier). Build the hand-rolled Lin-side bounds pass only for whatever IRCE leaves.

**1(a-ii) — null/union elision should reuse the checker's already-proven facts.** For the
non-bounds half (`?? default` null guards, `is T` union tags) don't re-derive facts in the IR:
`lin-check` *already computes* non-null flow, union narrowing, and exhaustiveness, then **discards
them before lowering**. Thread those facts into `lin-ir` so lowering simply omits the
provably-redundant guard. Near-zero new analysis, and it is a **separate, parallelizable lane**
from the bounds work above — different check kinds, different proof source, different files.

**1(b) — Inline the hot, simple runtime ops in codegen (emit IR, not a call).** Pick the few ops
that are short and hot:
- **RC retain/release**: emit the refcount inc/dec + `IMMORTAL_RC` guard + zero→free branch inline
  (extend `crates/lin-codegen/src/codegen/rc.rs`, which already has `emit_sealed_release`). Once the
  retain and release of a non-escaping borrowed read are both inline, `mem2reg`/`instcombine` can
  cancel the pair — something the Perceus `rc_elide` pass cannot prove but LLVM can after inlining.
- **box/unbox fast paths**: emit `lin_box_int32`/`lin_unbox_int32` as inline tag-write/tag-read so a
  box that flows straight into an unbox cancels across the now-visible boundary (the path-8 Tier-1
  "they don't cancel" failure was *because* the consumer stayed opaque — inlining removes that).
- **`lin_map_get` fast path**: inline `hash → probe first slot → compare`, with a call fallback on
  miss. The hot RAPTOR maps are high-hit; inlining the hit path removes a call and exposes the
  hash/compare to LLVM.

**1(b-i) — the LLVM mechanics that make inlining pay.** Inlining a runtime op only helps if LLVM
can then *scalarize* it. Mark the hot box/dispatch shims `alwaysinline` (inkwell `add_attribute`),
and emit any local box/dispatch scratch as an **entry-block** aggregate alloca — SROA/mem2reg
promote only non-escaping entry-block stack allocas, and the inliner restructures the entry block,
so alloca placement is load-bearing. Without this, the inlined body is absorbed but the allocation
survives.

**1(b-ii) — cancel box/unbox in the IR, not only in LLVM.** A complementary front-end route: make
`box`/`unbox` **first-class IR ops with a repr tag** (extending the ADR-062 repr tag) so a
box-flowing-straight-into-unbox cancels in `lin-ir` under copy-prop, *before* LLVM sees it. This is
cleaner than relying on inlined-runtime-op cancellation, and it is the substrate the value-axis ABI
work (Point 4) builds on. The inline-runtime-op path (the `lin_box_int32` bullet above) is the
fallback for pairs the IR can't statically cancel.

**1(b-iii) — make the surviving RC ops cheaper.** Inlining lets LLVM *cancel* RC pairs; it does not
cheapen the ones that survive. Because Lin deep-copies on thread transfer (ADR-028), nearly all
objects are provably thread-local, so the surviving retain/release can use **non-atomic** inc/dec —
a free constant-factor win on every RC op that does *not* cancel. (The Perceus *reuse/borrowing*
path is deliberately not here: PERFORMANCE.md marked it 0%/not-worth-it, and the better framing is
that inlining lets LLVM cancel what the Perceus pass cannot prove.)

**1(c) — Bitcode runtime + link-time merge. [blanket; follow-up only]** Compile `lin-runtime` to
LLVM bitcode (`.bc`), merge it into the user module before the O2 pipeline so the inliner sees
*every* runtime body. This is the path-8 Tier-1 experiment, measured **<2 % alone** — but that was
*before* a flat IV (1a's precondition) and internal linkage (Point 3) existed in the surrounding
code. Treat it as the general version of 1(a)/1(b): only worth it if the surgical, hand-picked
inlining shows signal, since the blanket form also bloats compile time and code size.

### Expected payoff / risk

This family is the **only** one that attacks the ~85 % axis directly. Honest caveat: every prior
piece came back small. The bet is the synergy in 1(a) (IV → inline check → dead → delete) and 1(b)
(inline RC → cancel). High effort, real soundness surface (an *incorrectly* elided bounds check is
a memory-safety bug — gate hard on the RC-balance verifier, ASan, and the digest). Start narrow
(1a against the fused range loop), measure, expand only on signal.

---

## Point 2 — closure-indirect combinator callbacks

### The problem (verified)

The hot *inner* loop (`range().for`) inlines its lambda body. But two cases still emit an **opaque
indirect call** the LLVM inliner cannot cross:

1. **Combinators with no inline path.** The inlinable-combinator set is `for`/`map`/`filter`/
   `reduce`/`while`/`some`/`every`/`find`/`flatMap` (each consulting
   `inlinable_capturing_lambda`, `crates/lin-ir/src/lower/combinator.rs:186`). `entries` is **not**
   in it — so RAPTOR's *middle* loop `queue.entries([routeId, stopP] => …)`
   (`raptor-algorithm.lin:71`) lowers its callback as a closure and calls it indirectly per entry.
2. **Callbacks that fall to the closure path.** When a callback is not an inlinable literal lambda
   (a stored/passed `Function` value, or captures that do not resolve), lowering takes
   `lower_callback_in_safe_ctx` (`combinator.rs:1899`) → `CallTarget::Indirect`, an all-ptr boxed
   ABI LLVM cannot see through (the box/unbox never cancels).

### Suggested fixes

**2(a) — Extend the inlinable-combinator set to `entries` / object iteration.** Give
`entries(map, [k,v] => body)` a flat-loop lowering that walks the `LinMap` slots, binds `k`/`v`, and
inlines the body when the callback is an inlinable literal lambda — the same machinery as `for`/`map`
(mirrors the range-for fusion fix). Removes the RAPTOR middle-loop indirect call. Also cover
`keys().for`/`values().for` object iteration. Bounded, well-scoped work.

**2(b) — Generalize callback devirtualization.** The Wave C `CallbackDevirt` mechanism
(`crates/lin-ir/src/monomorphize.rs`, currently `find`/`some`/`every`) specializes the combinator
body with a *statically-known* callback substituted in, turning the per-element boxed indirect call
into a direct call LLVM can then inline. Two concrete moves:
- **Land the existing branches**: `perf/devirtualize-barefn-combinator` (direct calls for
  `map`/`filter`/`for` with a bare named fn) and `perf/wavec-devirt-v3` (find/some/every,
  measured **2.54×** on the microbench). Both are complete and digest-clean per the branch notes.
- **Extend the spec axis** to *every* combinator and *any* statically-known callback target (named
  fn or a specific literal lambda we chose not to inline), not just the three short-circuiting ones.

**2(b-iii) — the long-horizon general form: Lambda Set Specialization. [if 2(b) shows signal]** The
spec-axis devirt above substitutes a *statically-known* callback. The principled generalization is
to make a callback's *lambda set* part of its type (Lambda Set Specialization, Brandon et al., PLDI
2023) and represent closures as `{tag, captured-fields}` tagged unions rather than the 48-byte
`{fn_ptr, env}` struct (`call.rs:12`). Then **capturing and anonymous** lambdas devirtualize too,
and — because the boxed-closure *value ceases to exist* — the box/unbox at `boxed_abi_wrapper_full`
(`call.rs:56-148`) becomes a dead pair LLVM deletes. Large but stageable: scope v1 to combinator
callback params, cap lambda-set size, keep the boxed ABI as the fallback for what it declines. This
is where 2(b) goes once the narrow form proves out; MLton reports up to 6.85× residual and Roc's
closure-as-tagged-union writeup is the implementation cookbook.

**2(c) — Guarded indirect-call promotion for genuinely-dynamic closures. [lower priority]** When a
stored closure has a small set of likely targets, emit `cmp fn_ptr, &known; direct-call fast path;
indirect fallback`. Self-built ICP (LLVM's own needs profile data). Only worth it where a dynamic
closure is genuinely hot and not devirtualizable by 2(b).

### Expected payoff / risk

Removes call **frames**. Per §5.8 the *work* inside (map_get/RC) remains, so on RAPTOR this mainly
fixes the `entries` middle loop; the larger beneficiary is the call-bound `interp` benchmark.
Medium effort, mostly existing infrastructure (land + generalize). Low-to-medium risk; the devirt
branches are already gated digest-clean.

---

## Point 3 — user functions have external linkage

### The problem (verified)

Codegen sets `Linkage::Internal` only on globals/string literals
(`crates/lin-codegen/src/codegen/mod.rs:599`, `literals.rs:71`); **user functions are left at
default external linkage** (no function `set_linkage` exists). External-linkage callees are still
inlined at their call sites within the module, but the inliner is more conservative — it cannot
*delete* the out-of-line body (a hypothetical external caller might exist) and cannot run
argument-level interprocedural opts. In a single-module whole-program compile that conservatism is
pure waste: there are no external callers.

### Suggested fix

**Whole-program internalization.** In the function pre-pass (where `compile_module_from_ir` calls
`module.add_function`), mark every user function `Linkage::Internal` **except**:
- `main` (the linker entry / C runtime calls it),
- any symbol genuinely needed across the final binary boundary (FFI-exported functions; anything in
  the `replace`/overload export machinery that an out-of-module consumer resolves — for a normal
  `lin build`, only `main` qualifies).

Address-taken functions (closure values via `MakeNamedClosure`, function-as-value) stay *defined*
because the address use keeps them live — internal + address-taken is fine. This lets the O2
inliner inline-and-delete, run arg-level IPO, and `globaldce` the leftovers.

### Expected payoff / risk

Modest but **free**, and it is the *enabler* for Points 1–2: the inliner won't aggressively absorb a
callee it cannot delete. Precedent: the memory's "RECORDS gap = idiv from external-linkage
val-global; internal linkage → parity" — the identical effect, extended from globals to functions.
Low risk; the only real constraints are keeping `main` and FFI/test-harness symbols external. Gate
on the full `cargo test --workspace` suite (catches any symbol a test binary or the `replace` mock
machinery actually needs externally).

---

## Point 4 — boxed values are materialized across the return ABI

### The problem (verified)

Points 1–3 attack *calls* by inlining and *checks* by eliding. Neither touches the **value
representation crossing a return**. A function returning `Trip | Null` (e.g. `getTrip`,
PERFORMANCE.md §2) allocates → boxes → returns a pointer → and the caller immediately unboxes —
four operations for one logical value, on the hot path, every iteration. Inlining the *call* does
not remove this: the materialization is in the value's ABI, not the call frame. This is the
value-axis cost the whole campaign kept measuring and that no inlining lever addresses.

### Suggested fix

**Worker/wrapper + Constructed Product Result (CPR) return-flattening.** Split each boundary
function into an unboxed **worker** — returns the record's fields, or a `(tag, payload)` pair, in
registers via LLVM's free multi-value `{i64, i64}` return ABI — and a thin inlinable **wrapper**
that preserves the old boxed signature for non-specialized callers. The worker/wrapper "rolling
rule" keeps the box/unbox conversion at the combinator entry, not per element. OCaml Flambda — the
closest industrial template (eager, AOT, value-records) — reports 20–30 % allocation reductions
from this family. Sources: Gill & Hutton (worker/wrapper, JFP 2009); CPR analysis; Peyton Jones &
Launchbury, "Unboxed values as first-class citizens" (the IR substrate Point 1(b-ii) lays down).

### Expected payoff / risk

The one lever **orthogonal to all three other Points** — it attacks the value ABI, not the call or
the check, so it can recover the `Trip | Null` materialization that survives a fully-inlined call.
Large effort; **hard RC-correctness gate**: the split/rebuild must *transfer* ownership, not
retain+release (a double-free/leak surface). Builds on Point 1(b-ii)'s first-class box/unbox ops.
Design may begin during Phase 1; it lands in Phase 2, gated on `LIN_VERIFY_RC` + ASan + digest.

---

## Point 5 — cross-cutting: profile-guided block layout (PGO)

### The problem / opportunity

The per-iteration safe-access checks are *almost-always-safe* — highly predictable branches — and
the devirt fallback arms (Point 2(c)) are almost-never-taken. The `default<O2>` pipeline lays these
out with static heuristics. Profile-guided optimization weights them from a real RAPTOR run, so hot
blocks fall through, cold guard arms split out, and the inliner prioritizes the hot path — exactly
the ~85 % call/value axis.

### Suggested fix

Wire instrumentation PGO (IRPGO): profraw → `llvm-profdata` → branch-weight metadata fed into the
existing pipeline. No hot-path *code* change, so it **composes with every other Point and can be
built fully in parallel**. Bonus: PGO the `lin` host compiler itself via `cargo-pgo`. Sources: LLVM
HowToBuildWithPGO; AutoFDO (Chen et al., CGO 2016).

### Expected payoff / risk

Independent, additive, low-risk; realistic band 5–10 % (Swift apps; AutoFDO geomean ~10.5 %). The
only cost is a profiling round-trip in the build. Re-profile at each measurement point. Note for the
cross-language *fairness* comparison: an absolute RAPTOR-wall win, but keep it off the like-for-like
table unless the other ports are similarly PGO-built.

---

## Sequencing plan — structured for maximum parallelism

The work splits into a few **enablers** that must land first, then **three file-disjoint attack
tracks** that run concurrently, then a **big-bet + blanket** phase gated on signal — with PGO
(Point 5) building in parallel throughout. The ordering principle is unchanged (exploit the synergy:
a real IV + internal linkage + a de-opacified accessor make every later step actually fire), but the
steps are now grouped so as much as possible runs in parallel worktrees **without colliding**.

### Dependency DAG

```
 PHASE 0 — enablers (all parallel)        PHASE 1 — attack tracks (parallel after M0)        PHASE 2 (on signal)
 ─────────────────────────────────        ───────────────────────────────────────────       ───────────────────
 E0.1 fusion ───┐                         CHECK   CK.1 IRCE/SCEV ──┐
 E0.2 linkage ──┼──► M0 ──►                (F,L,X)            CK.3 bounds-pass (cond. on CK.1)
 E0.3 accessor ─┘                                  CK.2 null/union (parallel, disjoint)
 E0.4 PGO  (independent, always-on)        RUNTIME RT.1 inline-mechanics ──► RT.2a RC
                                           (L)                          ├─► RT.2b box/unbox
                                                                        └─► RT.2c map_get
                                           CALL    CL.1 entries ┐
                                                   CL.2 land-devirt ──► CL.3 generalize ──► CL.4 LSS
                                                                                         └─► CL.5 guarded ICP
                                                          ──────────► M1 ──────────►  VA.1 worker/wrapper+CPR
                                                                                      BL.1 bitcode runtime
 (E0.4 PGO re-profiles at M0, M1, and Phase 2)
```

### Phase 0 — enablers (four parallel worktrees, all low-risk)

| Lane | Step | Owns (files) | Maps to | Why first |
|------|------|--------------|---------|-----------|
| **E0.1** | Range-for fusion + `is_provably_flat_producer` extension (`range`/rangeStep/`arrayAllocate`/`arrayAllocateFilled`) | `lin-ir` combinator detectors | Step 1 / `9f6108b3` | provides the `i32` induction variable |
| **E0.2** | Whole-program internalization (`Linkage::Internal` on all but `main`/FFI) | `lin-codegen/mod.rs` fn pre-pass | Point 3 | lets the inliner **delete** absorbed callees + run arg-level IPO |
| **E0.3** | Accessor de-opacification (inline `inbounds` GEP + single dominating bounds check) | `lin-codegen` data/array + `sba_oob` | Point 1(a-i) substrate | makes the check **visible IR** so CK.1 can see it |
| **E0.4** | PGO wiring (profraw → profdata → branch weights) | build/compile orchestration | Point 5 | fully independent; also speeds the host compiler |

**M0 (sync):** rebuild, full suite, RAPTOR digest + wall baseline. E0.1+E0.2+E0.3 together yield: a
real IV, deletable callees, and a visible bounds check — the preconditions Phase 1 needs.

### Phase 1 — three attack tracks (parallel after M0)

Each track is one owner/worktree; sub-lanes that touch disjoint files run in parallel inside a track.

**Track CHECK** — per-iteration safe-access (needs E0.1 + E0.2 + E0.3)
- **CK.1** [test-first, cheap] Enable IRCE + LICM/LoopUnswitch; measure whether LLVM's SCEV kills the now-visible bounds check against the IV. → Point 1(a-i)
- **CK.2** [parallel to CK.1, disjoint files] Null/union elision from the checker's discarded facts. → Point 1(a-ii). *Owns the `lin-check`→`lin-ir` fact plumbing.*
- **CK.3** [conditional on CK.1] Hand-rolled Lin-side bounds pass + `_unchecked` accessors for whatever IRCE leaves. → Point 1(a)

**Track RUNTIME** — opaque user-code→runtime ops (needs E0.2)
- **RT.1** [foundational] `alwaysinline` + entry-block-alloca discipline. → Point 1(b-i)
- **RT.2** [after RT.1, three disjoint sub-lanes in parallel]:
  - **RT.2a** RC inline + non-atomic RC — `codegen/rc.rs` → Point 1(b)/1(b-iii)
  - **RT.2b** first-class box/unbox IR ops + cancellation (fallback: inline box/unbox fast path) — `lin-ir` coerce + `codegen/boxing.rs` → Point 1(b-ii)
  - **RT.2c** `lin_map_get` fast-path inline — `codegen/data/index.rs` → Point 1(b)

**Track CALL** — residual indirect callbacks
- **CL.1** Extend the inlinable set to `entries`/`keys`/`values` object iteration — `lin-ir` combinator.rs → Point 2(a). *Serialize **after** E0.1 merges (same file).*
- **CL.2** [parallel to CL.1] Land existing devirt branches (`perf/devirtualize-barefn-combinator`, `perf/wavec-devirt-v3`, measured 2.54×) — `lin-ir` monomorphize.rs → Point 2(b)
- **CL.3** [after CL.2] Generalize the devirt spec axis to *all* combinators / *any* statically-known callback → Point 2(b)
- **CL.4** [after CL.3] Lambda Set Specialization (closures → `{tag, fields}`) → Point 2(b-iii) — large
- **CL.5** [after CL.3, low priority] Guarded indirect-call promotion for genuinely-dynamic closures → Point 2(c)

**M1 (sync):** integrate the three tracks; measure RAPTOR wall+digest **and** the `interp` cell
(Track CALL's bigger beneficiary).

### Phase 2 — big bet + blanket (on signal, after M1)
- **VA.1** Worker/wrapper + CPR return-flattening → Point 4 — large; design may begin during Phase 1; hard RC gate.
- **BL.1** Bitcode runtime merge → Point 1(c) — only if Track RUNTIME's surgical inlining showed signal.

### Conflict map (so parallel worktrees don't collide)

| Shared file / area | Lanes | Resolution |
|---|---|---|
| `lin-ir/.../combinator.rs` | E0.1, CL.1 | **serialize** — E0.1 merges first, CL.1 rebases on it |
| `lin-ir` coerce/lowering | CK.2, RT.2b | **one owner or explicit coordination** (both touch coerce/Index lowering) |
| `lin-codegen/mod.rs` | E0.2 (fn pre-pass), E0.3 (`sba_oob` region) | different regions; **rebase order E0.2 → E0.3** |
| `lin-ir` monomorphize.rs | CL.2 → CL.3 → CL.4 | single track owner, internally serialized |
| rc.rs / index.rs / boxing.rs / build pipeline / lin-check plumbing | RT.2a / RT.2c / RT.2b / E0.4 / CK.2 | single-owner, conflict-free |

Per CLAUDE.md worktree discipline: each lane is one branch, one agent, pinned to a confirmed master
base, **no merge without explicit sign-off**, and integration order follows the conflict map above.

### Gates (every step)

- **Correctness oracle**: RAPTOR digest must stay `group=26203913 range=773022892 journeys=139`.
- **Memory safety**: `LIN_VERIFY_RC=1` clean + an ASan run (1a/1b touch RC and bounds — an
  incorrectly elided check is a UAF/OOB, not a perf regression).
- **Full suite**: `cargo build --workspace && cargo test --workspace`, real exit codes (no
  pipe-masking), every `test result:` line `0 failed`.
- **Wall**: A/B the `lin-manually-typed` RAPTOR bench (old vs new compiler), native build, ≥3 runs,
  report median; do **not** run concurrently with the build (15–28 GB RSS).

### The decision this plan forces

**Phase 0 + Track CHECK CK.1 are the crux.** A real induction variable (E0.1) + internal linkage
(E0.2) + a de-opacified accessor (E0.3), then bounds-check elimination against that IV (CK.1, ideally
by LLVM's own IRCE) is the one combination the campaign has not tried, and it attacks the call/value
axis directly instead of the data layer. If it moves the wall clock, the rest of the program (Tracks
RUNTIME/CALL → Phase 2) is justified and the gap is reachable without a JIT. If it lands wall-neutral
like every data-layer lever before it, that is decisive evidence the remaining gap is *fundamental to
the execution model* (boxed-value-flow + safe-by-default semantics), and the only lever left is a
different backend (the JIT / whole-program specialization route in §5.8) — a multi-quarter decision,
made on data rather than hope. **The parallel structure means the other tracks can be built and
measured before that verdict is in**, so a wall-neutral CK.1 does not idle the team — and Point 4
(value-axis ABI) and Point 5 (PGO) are independent enough to pay even if CK.1 does not.
