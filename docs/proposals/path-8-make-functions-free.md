# Path 8 — Make functions free: demolish the call/closure/dispatch abstraction before *and* with LLVM

**Status:** Open proposal. The architectural framing + concrete current-ABI findings behind the
call/dispatch lever. **Complements [Path 6 (eliminate-call-dispatch-cost)](path-6-eliminate-call-dispatch-cost.md)**,
which proposes the IR-level inlining/fusion/monomorphization mechanisms (Tiers 2–3 here). This path adds
the missing **Tier 1** (the LLVM never gets to *see* the helpers — they're a linked `.a`, not bitcode)
and **Tier 4** (the loop forms), and the unifying model that makes the whole thing add up. **No userland
language change.**

**Direction in one line:** Lin is a functional language where *everything is a function* — `for`, every
combinator, every loop — and it is slow because those abstractions are still **standing as runtime
artifacts** (heap closures, boxed args, indirect calls, opaque helper calls) at the moment LLVM tries to
optimize. Fast languages (Rust, Zig, Go, C++) **demolish the abstraction before the optimizer runs**; Lin
hands LLVM the abstraction intact. The fix is to demolish it — in two places that *compound*.

---

## Why this path exists — the ceiling test says the cost is the *code*, not the data

Path 6's anchor measurement (H12, the `LIN_NO_RC`/arena ceiling test) is decisive: removing the **entire**
allocation + refcounting subsystem from interp gave **no speedup** (0.48 / 0.42 s vs 0.408 s baseline). So
the recoverable cost is not the data (alloc/RC/layout — Paths 1–5/7's targets) and not reads (already
const-offset). **It is the non-inlined calls.** And the one change that *did* move interp — Stage 1's
in-place `for`, ~3.2× — won by *removing per-element calls*, not by touching data. The code is the cost.

This path explains **why** the calls are so expensive (the current ABI, read from the codegen below) and
lays out the **full menu** of how to make them free — of which Path 6's IR-inlining is the middle tier.

## The architectural finding — what's actually happening (read from `crates/lin-codegen`)

Three facts about how Lin compiles today explain the entire call cost:

1. **The runtime is a linked static archive (`liblin_runtime.a`), NOT bitcode.** `lin_object_get`,
   `lin_box_int32`, `lin_unbox_int64`, `lin_tagged_arith`, `lin_string_concat`, … are **opaque external
   symbols** at optimization time. The codegen runs `module.run_passes("default<O2>", …)` over *user code
   only*; LLVM **cannot see into a single runtime helper** — cannot inline it, cannot cancel a
   `box(x)`→`unbox` round-trip (which today goes through *memory*), cannot fold arithmetic through
   `lin_tagged_arith`. Each helper is a hard call boundary wrapping a few instructions of real work.
   *(This is why the prior whole-program-LTO investigation found "no win" — the codegen-level inlining
   that LTO needed to act on was never there. This path is the targeted version.)*
2. **The closure ABI is uniform-boxed + indirect.** A closure is a 48-byte heap struct
   `{rc, _pad, fn_ptr@8, env_ptr@16, env_size@24, dflt_desc@32, cap_desc@40}` (`codegen/call.rs`). Every
   call is `builder.indirect_call(fn_ptr, …)` with **every argument boxed to a `TaggedVal*`** (the uniform
   ABI). LLVM **cannot devirtualize or inline an indirect call** — it does not know the target. So
   `range(0,n).for(f)` is, per element: box the index → indirect-call through `fn_ptr` → inside the
   callee, unbox → do the work → box the result → release. None of it visible to the optimizer.
3. **User functions are emitted with EXTERNAL linkage** (`module.add_function(name, ty, None)`). External
   functions must be preserved as callable symbols, which **throttles LLVM's own inliner** across user
   functions even when the target *is* statically known.

**So the problem is not "LLVM isn't good enough."** It is that the abstraction (heap closure, boxed args,
opaque helpers, indirect call, external linkage) is **erected at runtime and never torn down** before the
optimizer looks. Rust monomorphizes + inlines an entire `.map().filter().sum()` into one loop *before*
LLVM; Lin presents LLVM a tree of opaque indirect calls and asks it to optimize around them. It can't.

## The model — two demolition sites, and they *compound*

There are exactly two places to make "everything is a function" fast:

- **(A) Demolish it at the IR level, before LLVM.** Turn the call into straight-line code in `lin-ir`
  yourself, so LLVM receives a flat loop with no closure, no boxing, no indirect call — code it is
  brilliant at. (Stage 1's in-place `for`; Path 6's 6a/6b; Rust iterators.)
- **(B) Let LLVM demolish it.** Give LLVM what it needs to inline *itself*: the runtime as **bitcode**
  (see into the helpers), user functions as **internal linkage**, indirect calls turned into **direct**
  calls where the target is known.

**The critical insight: (A) and (B) multiply.** Today even Stage 1's perfectly-inlined loop *still* calls
opaque `.a` helpers, so half its potential win is left on the table — LLVM inlines the closure body but
then hits a wall of `lin_box`/`lin_unbox`/`lin_object_get` external calls it cannot cancel. Do (B) and the
box/unbox pairs an inlined loop produces finally **cancel**, the closure env **SROAs into registers**,
invariants **hoist**, arithmetic **folds**. (B) is what turns (A)'s output from "fewer calls" into "C-speed
straight-line code." Neither alone reaches the ceiling; together they do.

---

## The 4-tier menu (ranked by leverage × inverse-effort; all zero userland change)

### Tier 1 — Unblock LLVM (cheapest, widest blast radius, never tried)
- **Ship `lin-runtime` as embedded LLVM bitcode**, `llvm-link` it into the module *before* `run_passes`,
  and mark the hot leaf helpers (`lin_box_*`, `lin_unbox_*`, `lin_object_get`, `lin_tagged_arith`,
  small string/array accessors) `alwaysinline`/`inlinehint`. Effect: LLVM inlines the helpers and
  **cancels the box/unbox round-trips that today go through memory**, folds tag-known arithmetic, and SROA's
  the boxed values. This is the single highest value-to-effort action available and it has **never been
  done** (the runtime has always been a linked `.a`).
- **Internal linkage for all non-exported user functions** (`set_linkage(Internal)`), so LLVM's inliner
  is free to inline across them. Near-zero effort.
- *Risk:* keeping the bitcode in sync with the `.a` (build both from one source), DWARF/debug interplay,
  and that `alwaysinline` on a too-large helper bloats code — gate on size. ASan-clean + suite green.

### Tier 2 — Demolish the closure/combinator abstraction at the IR level (Path 6's 6a; the headline win)
- **Generalize the inlined-callback lowering** from `for`/`length` (shipped, Stage 1) to
  `map`/`filter`/`reduce`/`sortBy`: a literal/capturing closure becomes the loop body; the element is read
  in place; captures are read from the env by `load`, not via an indirect call. No per-element arg box, no
  indirect call, no per-element release.
- **Fuse combinator chains** (`xs.map(f).filter(g).reduce(h)`) into one pass — no intermediate arrays, no
  per-stage closure call (the Rust-iterator win).
- *Risk:* CFG/SSA correctness at the inline site (the latch-wiring hazard from prior fusion work) and the
  per-element element-box RC (Stage 1's leak — fixed by giving the fused read the concrete field type, not
  `Json`). ASan `detect_leaks=1`-with-scaling is mandatory (the Stage-1 lesson).

### Tier 3 — Kill the boxed ABI and the indirect call (deepest, highest ceiling)
- **Devirtualize statically-known call targets.** The overwhelmingly common case — `xs.map(literalLambda)`,
  a call to a named `val` function — has a *known* callee. Emit a **direct call**, not `indirect_call`, so
  LLVM can inline it. (Today everything goes through the boxed `{fn_ptr,env_ptr}` indirect path.)
- **Monomorphize the polymorphic stdlib per concrete element type** (`length$Int32`, `for$Trip`,
  `push$StopTime`) so there is **no shared boxed ABI to box into** — the boundary is *dissolved*, not
  special-cased. Lin already monomorphizes generics partially; the `get<T,D>` record-array link error is
  literally a missing instance.
- **A specialized unboxed calling convention** for monomorphic sites: pass scalars/records in registers or
  by `sret`, not as `TaggedVal*`. The biggest structural change and the highest ceiling — it attacks the
  boxing *at the call boundary itself*, which Tiers 1–2 only mitigate.
- *Risk:* monomorphization compile-time/binary-size cost and the machinery's history of biting
  (`mangle_type` collisions, the `get<T,D>` failure); the unboxed-convention ABI must interoperate
  correctly with the boxed fallback at every boundary (the recurring repr-mismatch class). Incremental,
  measured, ASan-gated.

### Tier 4 — The loop forms specifically ("a for loop is a function; a while loop is a function")
- `range(a,b).for(f)` already fuses to a counted loop (shipped, `23c3e1f`). Extend the same counted-loop
  recognition to the other loop-shaped combinators and to **`while`-as-recursion**.
- Make **self-tail-recursion** (already TCO'd to a loop) **compose with closure inlining**, so a recursive
  functional loop whose body calls an inlined closure becomes a flat LLVM loop with the body inlined — the
  functional-loop equivalent of Tier 2.

---

## Sequencing (the order matters because the tiers compound)

1. **Tier 1 first.** Low risk, and it *multiplies* the value of everything after it — without it, even
   perfectly-inlined loops still hit opaque helper calls. Cheapest thing with the widest blast radius, and
   it has never been done. **A prototype is in flight** (`spike/bitcode-runtime`): build the runtime to
   bitcode, link pre-`O2`, `alwaysinline` the hot helpers, internal-linkage user fns; measure box/unbox
   cancellation in IR + interp/RAPTOR wall-clock.
2. **Tier 2** (generalize combinator/closure inlining) — the proven mechanism (Stage 1's ~3.2×), the
   headline per-element win, now amplified by Tier 1's helper inlining.
3. **Tier 3** (devirtualize + monomorphize + unboxed convention) — the deep structural fix; highest
   ceiling, highest risk; incremental and measured.
4. **Tier 4** woven in where the loop shape is the bottleneck.

The in-flight RAPTOR profile tells us the *mix* — how much of the hot path is opaque-helper calls (favors
Tier 1) vs per-element closure calls (favors Tier 2) vs polymorphic dispatch (favors Tier 3) vs the
separate string-allocation frontier Path 6 flags — and therefore which tier leads for *that* workload.

## What this fixes / does not fix

- **Non-inlined call/dispatch + opaque-helper cost (#4):** yes — the measured recoverable cost. Tier 1
  unblocks the helpers; Tier 2 removes the per-element closure call; Tier 3 dissolves the boxed dispatch
  ABI; Tier 4 the loop forms.
- **Reads (#1):** already fast.
- **Boxing boundary (#2):** dissolved per-op by Tier 3's monomorphization; mitigated by Tier 1's box/unbox
  cancellation.
- **Construction/RC (#3):** not the target — the ceiling test says it is not recoverable for these
  workloads; Tiers 1–2 incidentally remove per-element arg-box alloc/RC as a side win.
- **The RAPTOR query-phase string-allocation frontier** (Path 6's companion note, ~3.5 B string allocs):
  **not touched** by this path — a separate frontier (string interning / avoiding per-access key
  construction).

## Relationship to the other paths

- **Path 6 is Tiers 2–3 of this menu** stated as standalone mechanisms; this path adds Tier 1 (the
  LLVM-can't-see-the-helpers finding) + Tier 4 + the compounding model. They should be read together; Path
  6 is the recommended primary direction and this is its architectural foundation and missing first tier.
- **Supersedes Paths 1/3/4/5/7 as the primary lever** for the headline benchmarks (the ceiling test shows
  their data/memory-model targets are not the recoverable cost). Those remain for niche cases (Path 5 for
  array-of-struct *bandwidth*; Path 7 only if RAPTOR is measured allocation-bound unlike interp).
- **Composable with Path 2 (inline caches)** for residual genuinely-`Json` reads.

## Acceptance gates

Full `cargo test --workspace` green; the combinator/iterator/stream corpus green (the call-heavy suites are
the guard); RAPTOR digest byte-identical (`group=26203913 range=773022892 journeys=139`); **ASan
`detect_leaks=0` AND `=1`-with-scaling** on any fused/inlined combinator (Stage-1-leak lesson); the
**mechanism in IR** (Tier 1: box/unbox pairs cancelled, helper bodies inlined; Tier 2/3: no per-element
indirect closure call / no shared-ABI box in a typed hot path) **and** the wall-clock (median ≥5 at low
load — the box is contended). Tier 1 specifically must show the runtime-bitcode link is debug-info-clean
and does not regress build time unacceptably.

## Verdict

Lin is slow because "everything is a function" is compiled as "everything is a *runtime artifact LLVM
can't see through*" — a linked-`.a` opaque helper, a boxed indirect closure call, an external-linkage
function. The fix is to demolish the abstraction in two compounding places: **let LLVM demolish it** (Tier
1: bitcode runtime + alwaysinline + internal linkage — cheap, never tried, multiplies everything else) and
**demolish it yourself at the IR level** (Tiers 2–4: inline/fuse combinator+closure calls, devirtualize +
monomorphize the dispatch, flatten the loop forms — Path 6's mechanisms, proven once at ~3.2×). This is
exactly how Rust/Zig/Go reach C speed: the functional design is not the enemy; *leaving the abstraction
standing at runtime* is. Zero userland change. Start with Tier 1 (in-flight spike), then generalize Tier 2,
then go deep on Tier 3 — sequenced by what the profile says dominates.
