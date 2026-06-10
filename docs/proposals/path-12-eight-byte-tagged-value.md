# Path 12 — Shrink the dynamic value: 8-byte tagged representation + borrowed reads

**Status:** Open proposal, **sequenced after [Path 10](path-10-layout-as-a-type-system-fact.md)** —
re-measure the seam profile once packing has deleted most dynamic reads, then decide go/no-go on the
representation migration. **No userland language change.** This path owns the *remaining* dynamic
seams (`Json`, unions, map containers) after Paths 10/11 have made the statically-shaped majority
seam-free.

**Direction in one line:** replace the 16-byte heap-boxed `TaggedVal` with an **8-byte immediate
tagged value** (NaN-boxing or low-bit tagging) so ints/floats/bools/null/pointers fit in one register
with zero allocation and half the memory traffic, and make dynamic reads **borrowed by default** so
the read wrapper stops paying an owning clone per access.

---

## 1. The measured target

- `TaggedVal` is `{ tag: u8, pad: [u8;7], payload: u64 }` — 16 bytes
  (`lin-runtime/src/tagged.rs:40`), and dynamic values are **heap-boxed pointers to it**: every
  dynamic operation pays a pointer indirection plus double the cache traffic of an 8-byte value, and
  every produced scalar pays a box (malloc or small-int-cache hit).
- RAPTOR query phase: **~3.5 B box ops** (TAGGED_RELEASE 1.37 B / CLONE 1.12 B / ALLOC 1.02 B). The
  dominant historical leak class (box-shell-of-fresh-heap-val, operand box shells, union-arith result
  boxes — see [[project_raptor_slowness_leaks]]) exists *only because the shell exists*.
- **The malloc is not the cost** — this is the load-bearing negative result. The 16-byte box pool was
  3-4% *slower* ([[project_tagged_box_pool_negative]]); the LIN_NO_RC ceiling recovered ~0%
  ([[project_gc_retired_not_alloc_bound]]). What's left is **width + indirection + the ownership
  churn around each box** — exactly what a representation change addresses and allocator tweaks don't.
- Path 2's inline caches independently isolated the same wrapper cost: a 99.56%-hit-rate IC was still
  a wash because the read's expense is **key-interning + unboxing + tag-dispatch + the owning clone**,
  not offset resolution. The fix for the wrapper is to shrink the value and stop cloning, not to cache
  the offset.

## 2. The technique and the evidence

Every production JS engine converged on 8-byte values: JSC uses NaN-boxing, V8 and SpiderMonkey use
tagged 8-byte representations; surveys find tag/untag cost roughly a wash between schemes, with the
consistent result that **value width dominates** (one register, half the cache lines —
[Core Dumped survey](https://coredumped.dev/2024/09/09/what-is-the-best-pointer-tagging-method/),
[NaN-boxing writeup](https://piotrduperas.com/posts/nan-boxing/)). OCaml has run on a 1-bit-tagged
8-byte value for thirty years. The strongest recent measurement is **float self-tagging
([POPL'25](https://arxiv.org/pdf/2411.16544))**: superimposing tags on common float bit-patterns
unboxes essentially all floats — **2.3× on float-heavy benchmarks, no regression elsewhere**.

For the borrow half: Lean's Perceus-family compiler infers **borrowed vs owned** parameter positions
("Counting Immutable Beans", [arXiv:1908.05647](https://arxiv.org/pdf/1908.05647)) so reads that don't
escape pay zero RC and zero clone. Lin's `lin_object_get` currently returns a value the caller must
own (the projection-clone — `index_result_is_fresh_owned_box` and friends), which is correct under
the current ownership discipline but pays a clone+release per read on paths that only inspect.

## 3. Mechanism

### 3a. Choose the encoding (decide by measurement, both are viable)
- **NaN-boxing:** doubles are immediate; ints (≤51-bit), bools, null, and pointers live in the NaN
  payload space. Best when floats are hot.
- **Low-bit tagging:** pointers are aligned so low bits carry the tag; small ints are
  `(i << shift) | tag`; doubles box (or self-tag per POPL'25). Best when pointers/ints dominate —
  which Lin's profile suggests (RAPTOR is int/string/record-heavy).
- Either way the dynamic value becomes a **by-value u64**, not a pointer to a heap shell. `Int64`
  values that don't fit the immediate range box, as in every engine; Lin's numbers are
  overwhelmingly small (the existing small-int cache covering [-128, 1024) is the evidence).

### 3b. What it deletes
- `lin_box_int*/float*/bool` allocation and the small-int caches (the immediate *is* the value).
- The box-shell RC: retain/release on scalar boxes disappears as a category, and with it the operand
  box-shell leak class that has cost multiple debugging campaigns.
- Half the memory traffic on every tagged array element (`LinArrayElem` 16 → 8 bytes), every map
  value, every union payload slot.

### 3c. Borrowed reads
With [Path 13](path-13-ownership-parameter-conventions.md)'s conventions in the IR, `lin_object_get` /
map-get / array-get-tagged return **borrowed** values; the caller retains only on escape (store,
capture, return). The projection-clone moves from "every read" to "every escaping read" — on RAPTOR's
profile (inspect-and-compare loops) that is the minority.

### 3d. Migration shape
The runtime intrinsic ABI changes wholesale (`*const TaggedVal` → `u64` by value), which is why this
is foundations-now work: every `lin_*` symbol touched once, mechanically, behind the
`BuilderExt`/`RuntimeFns` façade that already centralizes the declarations
(`codegen/runtime.rs`). Small-string optimization (inline ≤15 bytes, Swift/Nim-style) and universal
interned-key comparison ride the same runtime sweep — every concat today is a malloc
(`LinString` has no SSO, `string.rs:6`).

## 4. Staged plan

1. **Re-profile after Path 10 stage 4** (packed default ON): count residual box ops / dynamic reads on
   RAPTOR + interp. **Go/no-go gate:** if the remaining seam traffic is <10% of runtime, stop here —
   the migration doesn't pay.
2. **Encoding spike:** implement the chosen encoding for scalars only (int/bool/null immediate;
   strings/objects/arrays stay pointers) behind a runtime-wide compile-time switch; A/B the
   `benchmarks/compare` suite.
3. **Full migration:** all tagged slots (array elems, map values, union payloads) to 8-byte; delete
   the shell allocator, the caches, the `is_cached_box` checks.
4. **Borrowed reads** (gated on Path 13's verifier): flip projection intrinsics to borrow-return;
   retain-on-escape inserted by lowering; ASan + the full UAF regression corpus is the gate — this
   touches the exact seam where the historical UAF class lives, which is why it *must not precede*
   Path 13.
5. **Float self-tagging** as a follow-up only if a float-heavy workload appears (none of the current
   benchmarks are).

## 5. Risks
- **Blast radius:** every runtime intrinsic signature changes. Mitigation: the façade pattern already
  in place, one mechanical sweep, and the compile-time switch keeping old/new builds A/B-able through
  stage 3.
- **Int64 range split:** full-width `Int64` values outside the immediate range must box; codegen must
  not assume immediates. (OCaml/V8 precedent: rare path, cheap guard.)
- **The go/no-go is real:** Paths 10/11 may shrink the seams enough that this path's payoff is small.
  That outcome is fine — stage 1 is one profiling run, and the borrowed-reads half (3c) pays
  regardless because it also serves packed-record heap-field reads.
- Cross-thread transfer (`deep_clone`) and `Shared<T>`'s atomic box must be re-audited under the new
  encoding (tag bits must survive the transfer walk).

## 6. Relationship to other paths
- **After [Path 10](path-10-layout-as-a-type-system-fact.md)** (don't optimize seams about to be
  deleted); **gated on [Path 13](path-13-ownership-parameter-conventions.md)** for borrowed reads.
- **Orthogonal to [Path 11](path-11-lambda-set-specialization.md)** — Path 11 removes boxing at
  *specialized* call edges; this path cheapens whatever dynamic edges remain (⊤ lambda sets, `Json`).
- Supersedes the dead Path-2 inline caches as "the dynamic-seam strategy": same target, but attacks
  the measured cost (wrapper + width) instead of the measured non-cost (offset resolution).
