# Path 11 — Lambda-set specialization: one general closure-devirt pass

**Status:** Open proposal. **The successor to [Path 6](path-6-eliminate-call-dispatch-cost.md) /
[Path 8](path-8-make-functions-free.md) Tier 2-3** — it replaces the per-combinator
`combinator_intrinsic` mechanism with the general pass those paths' own findings say is missing. **No
userland language change.**

**Direction in one line:** track, in the checker, the *set* of lambdas that can flow to each
function-typed parameter; specialize every higher-order function per lambda set (singleton set → direct
call + inlinable body, small set → switch over unboxed environment variants); closure environments
become per-set unboxed structs instead of heap-boxed uniform `(ptr env, ptr boxedArgs...) -> ptr`.

---

## 1. The measured target

- interp is **call-bound**, not data-bound: the H12 ceiling test (all alloc+RC no-ops) recovered ~0%,
  while combinator fusion (6a) bought ~3.3× on chains and the in-place `for` redirect ~3.2× — removing
  *calls* is what pays on this profile.
- The current mechanism is **ad-hoc per-combinator lambda-set specialization with singleton sets**:
  eta-expansion `f → (p) => f(p)` into `try_inline_combinator_wrapper` + `combinator_intrinsic` covers
  exactly `map`/`filter`/`reduce`/`for`/`while`. The frontier memo
  ([[project_combinator_inline_frontier]]) names what's left and why the trick doesn't extend:
  `find`/`some`/`every`/`takeWhile`/`flatMap`/`partition`/`scan`/`groupBy`/`sortBy`/binary-search all
  call the user callback **from inside a nested closure**, so they stay indirect — and concludes the
  real lever is "a general no-capture-closure devirt pass (doesn't exist)". This path is that pass,
  generalized to *with*-capture closures.
- Path 8's Tier-3 devirt spike closed-negative for *named* calls (already direct); the only indirect
  calls left are precisely these callback positions inside stdlib combinator bodies — i.e. the residual
  indirection is 100% lambda-set-shaped.
- Every closure call today pays the uniform boxed ABI
  (`lin-codegen/src/codegen/call.rs`): box each scalar arg (malloc or small-int cache), indirect call
  through the env, unbox inside the wrapper, box the result, unbox at the caller — 2-3 boxings and two
  representation round-trips per element where Go does one add instruction.

## 2. The technique (and why it fits Lin specifically)

**"Better Defunctionalization through Lambda Set Specialization" (PLDI'23,
[doi 10.1145/3591260](https://dl.acm.org/doi/10.1145/3591260))** — the design Roc adopted as "lambda
sets" ([roc#5969](https://github.com/roc-lang/roc/issues/5969)). The type system annotates each
function type with the set of syntactic lambdas that can inhabit it; higher-order functions are
specialized per set; a closure value becomes an unboxed tagged union of the set's environments; the
call becomes a direct call (singleton) or a jump table (small set). Reported speedups: up to **6.85×
under MLton, 3.45× under OCaml, 78.93× under Morphic**. The classical ancestor is MLton's
whole-program defunctionalization via 0CFA (historically 2-6× over separate compilation,
[Weeks 2006](http://www.mlton.org/References.attachments/060916-mlton.pdf)); the PLDI'23 formulation
is **type-directed**, which is why it fits Lin:

- Lin already monomorphizes generics in the checker — lambda sets ride the same propagation
  machinery (a set annotation on `Type::Function`, unified like any other type component).
- No whole-program 0CFA pass is needed; the checker's existing flow does the work.
- It **handles captures**: each lambda's environment is a known struct type; the per-set union of
  environments is unboxed and stack-allocatable. This is what the `combinator_intrinsic` trick could
  never do, and it incidentally removes the escaping-capture heap machinery from the hot path — the
  same machinery implicated in the obj-literal-closure-var and worker-captured-var bug class.
- It subsumes the whole remaining combinator frontier **in one mechanism** instead of one intrinsic
  per combinator, and it deletes none of the existing fusion work — fused chains simply see direct
  calls where they used to see indirect ones, widening what 6a's fusion can inline.

## 3. Mechanism

### 3a. Checker: lambda-set inference
Extend `Type::Function` with a lambda-set component (a set of lambda ids, or ⊤ for genuinely unknown —
FFI boundaries, values read from `Json`, worker-transferred closures). Literal lambdas and named
functions used as values contribute singletons; unification unions the sets; generalization carries
them on the monomorphization key, so a generic higher-order function specializes per (type args ×
lambda set), exactly like today's per-type monomorphization.

### 3b. Lowering: per-set closure representation
For a singleton set with no captures: the closure value compiles to *nothing* (the call site calls the
function directly — today's eta-expansion result, now universal). Singleton with captures: the env is
that lambda's concrete struct, passed unboxed. Small set (≤ a threshold, say 8): unboxed tagged env
union + switch at the call site. ⊤: today's uniform boxed ABI, unchanged — the escape hatch keeps FFI,
`Json`-stored, and cross-worker closures working byte-identically.

### 3c. Codegen: unboxed calling convention for specialized calls
A specialized higher-order function's callback parameter is no longer `(ptr env, ptr boxed...) -> ptr`
— it takes the concrete unboxed param/return types of the set's signature. The box/unbox round-trip
per element disappears at the source, rather than relying on LLVM to cancel it later
(the bitcode-runtime spike proved it cannot while the call is indirect —
[[project_bitcode_runtime_spike]]).

## 4. Staged plan

1. **Shadow inference:** compute lambda sets in the checker, emit statistics
   (`LIN_COUNT`-style: % of call sites singleton / small / ⊤ on interp + RAPTOR + stdlib tests).
   Expectation from the frontier memo: interp's hot callback sites are overwhelmingly singleton. No
   behaviour change; this validates payoff before any lowering work.
2. **Singleton, no-capture:** direct-call lowering for singleton sets (supersedes eta-expansion;
   `try_inline_combinator_wrapper` becomes a special case and is deleted). Gate: full suite + ASan +
   no-scaling-leak + interp benchmark.
3. **Singleton, with-capture:** unboxed concrete env, stack-allocated when non-escaping (composes with
   the existing escape analysis from Path 3's B2 work). This is the step that unlocks
   `find`/`some`/`every`/`flatMap`/`partition`/`groupBy`/`sortBy`.
4. **Small-set switch dispatch** + per-set specialization of generic stdlib higher-order functions.
5. **Unboxed calling convention** for all monomorphic specialized calls (scalars and
   [Path 10](path-10-layout-as-a-type-system-fact.md)-packed records in registers).

## 5. Risks
- **Set explosion / compile-time:** mitigated by the small-set threshold (⊤ fallback) and by the same
  dedup machinery monomorphization already uses. PLDI'23 reports compile-time stayed manageable; Roc
  ships it in production.
- **The ⊤ seams:** closures stored into `Json`, sent across workers (deep-copy transfer), or crossing
  FFI must demote to the boxed ABI; the conversion (wrap a specialized closure in a boxed-ABI thunk)
  is the same wrapper codegen already emits today, kept for exactly these seams.
- **Recursion through function-typed record fields** can make sets recursive; the standard answer
  (widen to ⊤ at the recursive knot) is sound and matches Roc.
- **Module boundaries:** the set must serialize through the `.lin-cache` signature like any other type
  component (same cache-format bump as Path 10's layout stamp — do them together).

## 6. Relationship to other paths
- **Subsumes** Path 6's 6a/6b frontier and Path 8 Tier 3; keeps their shipped wins.
- **Independent of [Path 10](path-10-layout-as-a-type-system-fact.md)** — different bottleneck
  (interp's calls vs RAPTOR's representation); can proceed in parallel. They meet at the Phase-0
  convergent finding (both benchmarks bottleneck on boxed heap-field records at the *edges* of calls),
  so the unboxed calling convention (stage 5) wants Path 10's packed layouts as parameter types.
- **Feeds [Path 14](path-14-whole-program-spine.md):** once calls are direct, runtime-bitcode LTO can
  finally cancel box/unbox pairs across them — the exact condition the <2% spike result was waiting on.
