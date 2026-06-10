# Path 13 — Ownership as IR fact: borrow/own/inout parameter conventions + a call-edge verifier

**Status:** Open proposal. Part performance, part **the structural fix for the recurring RC bug class**
that taxes every other path's schedule. **No userland language change** — conventions are inferred and
internal; one allowed strictness tweak at mutation/aliasing seams (§4).

**Direction in one line:** make ownership a **declared fact on every IR function signature** —
each parameter and return is `borrow` (caller keeps ownership, callee may not store), `own`
(transferred), or `inout` — inferred by lowering, consumed by codegen, and **checked by a verifier at
every call edge**, so the UAF/double-free/leak class becomes a compile-time IR error instead of an
ASan campaign, and defensive retain/release disappears wholesale.

---

## 1. The diagnosis: one bug class, many costumes

The project's RC bugs are not independent incidents; they are the same structural gap recurring at
every new seam, because **ownership is an emergent property of scattered retain/release emission, not
a checked contract**:

- the TCO param-slot class: old owned param values overwritten without release (`9a1a735`), body-scope
  temps released in the unreachable `tco_post` block (`b2e6d35`), the sealed-record param-slot
  carve-out leak (`codegen/types.rs:131`, still pinned);
- the union-boundary class: union-match-narrow retain, union-boundary double-free
  (`record_escape_alias` + `own_for_read`), the `Trip|Null` TCO UAF (`object.rs:218`) that made the
  9C face-2/seal-prop combination unsound;
- the projection class: `arr[i]` re-CloneBox of an already-fresh +1 box, projection-aliasing UAF in
  kConnections, escape-alias over-keep (leak #5);
- the boundary-coercion class: `lower_coerce_arg` unbox double-free, worker-transfer misaligned deref.

Each fix is correct and local; none prevents the next instance, because nothing *states* the contract
each call edge is supposed to satisfy. `docs/MEMORY_MANAGEMENT.md` and
[[project_rc_ownership_invariants]] document the borrowed-vs-owned rules **in prose** — this path
moves them into the IR where a verifier can enforce them.

There is also a direct performance half: because callee intent is unknown, every call boundary pays
**defensive** RC and defensive boxing. Path 3's measurement put the call-boundary cliff at **~13×**
(identical loop, RC-suppression on/off made no difference *because the boundary, not the RC, was the
cost*); Lobster's compile-time ownership analysis eliminates **~95% of RC ops** by exactly this kind
of inference ([Lobster memory management](https://aardappel.github.io/lobster/memory_management.html)).
And the not-alloc-bound finding does **not** make this a no-op: RC traffic isn't the cost, but the
*ownership churn* — clones taken to satisfy unknown callees, boxes owned defensively — is part of the
3.5 B box-op wrapper cost that Paths 10/12 attack; conventions are what let those paths' borrowed
fast paths exist soundly.

## 2. The technique and precedents

This is Swift's parameter-convention model (`@guaranteed`/`@owned`/`inout` in SIL, checked by the SIL
verifier), Lean's borrow inference ("Counting Immutable Beans",
[arXiv:1908.05647](https://arxiv.org/pdf/1908.05647)), and Koka/Perceus's ownership tokens — all
internal, all invisible at the surface. Hylo's `let/inout/sink/set` is the same algebra made
user-facing ([MVS paper](https://arxiv.org/abs/2106.12678)); Lin keeps it internal. The common result:
RC/copy operations exist only where a convention demands them, and a convention mismatch is a
compiler bug caught at compile time, not a heap corruption caught (if lucky) by ASan.

## 3. Mechanism

### 3a. Conventions on `LinIR` signatures
Every IR function (user, stdlib, intrinsic, closure wrapper) declares per-param and per-return
conventions. Defaults by inference in `lin-ir` lowering:
- a param only read and never stored/captured/returned → `borrow`;
- a param stored into a heap location, captured, or returned → `own`;
- the runtime intrinsics get **hand-audited declarations once** in `codegen/runtime.rs`'s
  `RuntimeFns` (e.g. `lin_object_get(borrow, borrow) -> borrow`,
  `lin_push(inout, own)`, `lin_array_get_tagged(borrow, _) -> own` — encoding today's prose rules).

### 3b. The verifier
A pass over `LinIR` checks, per call edge and per block: every `own` argument has an owned source
(fresh, cloned, or transferred — and is dead after the call unless re-owned); every `borrow` argument
outlives the call; returns match declared convention; **TCO back-edges release the old owned
param-slot value before the store** (the entire `tco_post` class becomes "verifier rejects emission
into an unreachable block"); scope exits release exactly the owned-and-live set. Run it in debug/CI
builds the way the repr verifier runs today.

### 3c. Codegen consumes conventions
`emit retain/release` decisions stop being per-site heuristics (`own_for_read`,
`record_escape_alias`, `index_result_is_fresh_owned_box`, the `tco_owns` flag + runtime
alias-compare) and become mechanical reads of the convention. The existing Perceus-style
`rc_elide.rs` pass keeps running afterwards, but most of its work disappears at the source — pairs
that were never emitted don't need eliding.

## 4. The allowed strictness tweak
A `borrow`ed projection that *escapes* (stored, captured, returned) needs an explicit ownership
upgrade. Lowering inserts the retain automatically — no user-visible change in the common case — but
two dark corners become defined instead of accidental: (a) aliased mutation through a projection held
across a mutation of its source (today's behaviour is whatever the RC emission happened to do; under
conventions it is a defined retain-on-escape), and (b) the escaping-`var`-capture class
(obj-literal closure-var segfault, worker captured-var garbage —
[[project_objlit_closure_var_segfault]], [[project_worker_captured_var_escape]]) gets a *checked*
capture convention instead of a latent miscompile. Both are behavioural tightenings of currently-buggy
corners, within the "variable referencing" allowance.

## 5. Staged plan

1. **Declare + verify, change nothing:** add convention fields to IR signatures, hand-write the
   `RuntimeFns` table, infer the rest, and run the verifier in **shadow mode** over the full test
   corpus + RAPTOR + examples. Every violation it reports is either a latent bug (file it — the two
   pinned TCO/union leaks should fall out immediately) or a wrong inference (fix the inference).
   Zero behaviour change; pure debt-finder, exactly like Path 10's stage 2.
2. **Make codegen consume conventions** for the easy classes first (scalar/borrow params), deleting
   the per-site heuristics they replace, one heuristic at a time, ASan-gated.
3. **TCO under conventions:** re-derive the tail-call release discipline from the verifier rules;
   this *is* the principled fix for the pinned sealed-record param-slot leak rather than another
   carve-out patch.
4. **Borrowed returns** from projection intrinsics (jointly with
   [Path 12](path-12-eight-byte-tagged-value.md) stage 4): retain-on-escape inserted by lowering,
   verifier proving the borrow never outlives its source.
5. **Measure**: RC-op counts (expect a Lobster-like collapse), the call-boundary microbench from
   Path 3, and — the real KPI — **zero new UAF/leak incidents across the next two perf campaigns**.

## 6. Risks
- **The hand-audited `RuntimeFns` table is load-bearing:** a wrong intrinsic declaration is a
  miscompile. Mitigation: stage 1's shadow mode cross-checks declarations against the prose rules and
  the existing test corpus before anything changes; the table is ~100 lines reviewed once, versus the
  current state of the same facts distributed across every call site.
- **Inference conservatism:** when in doubt, infer `own` (today's behaviour) — correctness never
  regresses, only the win shrinks.
- **`var` capture cells and `Shared<T>`** sit outside the simple algebra (shared mutable slots);
  they keep their current explicit-cell semantics and the verifier treats the cell as the owner.
- This path adds compile-time work (the verifier); keep it debug/CI-only if release compile time
  matters.

## 7. Relationship to other paths
- **Start early; everything composes with it.** [Path 10](path-10-layout-as-a-type-system-fact.md)
  says what shape a value is; this path says who owns it — together the packed/boxed-mismatch UAF and
  the ownership UAF classes are both unrepresentable.
- **Gates** [Path 12](path-12-eight-byte-tagged-value.md)'s borrowed reads (stage 4 there = stage 4
  here).
- **De-risks the schedule itself:** the dominant calendar cost of Paths 1/6/9 was ASan triage of this
  bug class; the verifier converts that to compile-time errors, which is a velocity multiplier for
  every subsequent path. ("Engineering velocity is a performance strategy" is the honest framing.)
