# Design: Closing the `interp` call-axis gap

**Status:** design investigation, read-only  
**Scope:** the `interp` benchmark (363 ms Lin, 42 ms Node, 12 ms Rust) and any program with
hot recursive calls over per-frame heap allocations.  
**Measured root cause (PERFORMANCE.md §4, line 352-358):** ~82 % of interp runtime is
*object allocation + RC at call/value boundaries* — per-frame `Cursor`/`Token` heap allocs
and the ~1 000 retain/release sites they generate — not dispatch, not representation, not
strings. The sealed-struct Cursor experiment *regressed* 9 %: unboxing is not the same as
not allocating. The lever is **alloc elimination + RC elision**, not a wider packing gate.

---

## 1. Call lowering: where Indirect is chosen and what it costs

### CallTarget variants

Defined at `crates/lin-ir/src/ir.rs:589-594`:

```
CallTarget::Direct(FuncId)   — callee is a known compile-time function id
CallTarget::Indirect(Temp)   — callee is a runtime closure value in a temp
CallTarget::Named(String)    — callee is an external / runtime symbol
```

`Indirect` is chosen when the callee is a runtime value: a closure stored in a local slot,
passed as a parameter, or extracted from a container. Every indirect call goes through
the **uniform boxed-closure ABI**.

### Boxed-closure ABI layout

A closure struct is 48 bytes (`crates/lin-codegen/src/codegen/call.rs:12-16`):

| offset | field | size |
|--------|-------|------|
| 0 | rc | i32 |
| 4 | _pad | i32 |
| 8 | fn_ptr | ptr → `__cls_wrapb_*` |
| 16 | env_ptr | ptr |
| 24 | env_size | i64 |
| 32 | default-arg descriptor | ptr |
| 40 | capture descriptor | ptr |

The `fn_ptr` stored in every closure (including capture-less named functions) points to a
**`__cls_wrapb_<fn>` wrapper** (`call.rs:56-153`). This wrapper has the uniform signature:

```
(ptr env, ptr boxedArg…) → ptr
```

All arguments arrive as `ptr` (TaggedVal*); the return is always `ptr`. Inside the wrapper
(lines 119, 144):
1. Each incoming `ptr` arg is **unboxed** to the concrete parameter type via `unbox_value`.
2. The real function is called with unboxed native-typed args.
3. The return value is **boxed** back to a `TaggedVal*` via `box_value`.

### Indirect call at the codegen level

At `crates/lin-codegen/src/codegen/mod.rs:1341-1382`, an `Indirect` call:
1. Loads `fn_ptr` from closure offset 8 (`struct_gep …, 2`).
2. Loads `env_ptr` from closure offset 16 (`struct_gep …, 3`).
3. Boxes each argument via `box_arg_for_closure_abi` (line 1363).
4. Emits an LLVM `indirect_call` with signature `(ptr, ptr…) → ptr` (line 1375).
5. **Unboxes** the returned `ptr` via `unbox_tagged_val_to_type` (line 1381).

So one logical call `f(item)` with `Int32` arg and `Int32` return expands to:
- box arg → `lin_box_int32(item)` → 16-byte TaggedVal*
- indirect_call fn_ptr(env, boxed_arg) → ptr
- unbox result → `lin_unbox_int32(result)` → i32

LLVM cannot see through the indirect call, so the box/unbox never cancels.

### Direct call

For `Direct(fid)` (line 1148-1155), codegen emits a plain `call <concrete_ret_ty>
@<fn_name>(<concrete_arg_tys>)` — no boxing, no indirection. LLVM inlines at O2.

### TaggedVal layout and box/unbox instruction counts

`TaggedVal` is 16 bytes (`crates/lin-runtime/src/tagged.rs:40-57`):
```c
struct TaggedVal { u8 tag; u8[7] _pad; u64 payload; }  // sizeof=16, alignof=8
```

Instruction cost of `box_int32` (`boxing.rs:59-63`):
- Cache hit (values in [-128, 65536)): ~5-7 LLVM instructions (compare + conditional branch +
  load from static table).
- Cache miss: ~15-20 instructions (allocator call + 2 stores + padding zero).

Instruction cost of `unbox_int32` (`boxing.rs:268-272`): ~5-7 instructions (2 loads + tag
check + optional truncate).

Combined round-trip per element: 12-27 instructions depending on cache state.

---

## 2. Monomorphizer devirt: what exists and what the ceiling is

### SpecKey and generic specialization

`type SpecKey = (usize, Vec<(u32, String)>, Option<CallbackDevirt>)`
(`crates/lin-ir/src/monomorphize.rs:530`).

The three axes are:
1. **Type-arg axis** — one specialization per distinct concrete type substitution for each
   quantified TypeVar. Clones the body, substitutes TypeVars, renames inner closures (to
   prevent symbol collision across axes), re-homes cross-module bodies, then re-monomorphizes
   nested generic calls (worklist fixpoint, lines 783-1090).
2. **Combinator-inline axis** (`try_inline_combinator_wrapper`, lines 2611-2703) — detects
   when a generic call is a thin wrapper over a stdlib combinator intrinsic (`lin_map`,
   `lin_filter`, etc.) and the callback argument is a literal lambda. Rewrites the call to go
   directly to the intrinsic slot, bypassing the wrapper. Layer-2 (the lowerer's combinator
   path in `crates/lin-ir/src/lower/combinator.rs`) then splices the lambda body inline if
   its captures are resolvable, achieving zero-allocation fusion.
3. **CallbackDevirt axis** (Wave C, `CallbackDevirt` struct, lines 1808-1818) — when a
   combinator's callback argument is a **named no-capture function** (a bare module-level
   symbol like `find(arr, isEven)`), adds a third SpecKey axis. The specialization body
   substitutes all reads of the callback parameter with direct references to the named
   function, then arity-truncates the call if needed. Result: the per-element indirect call
   becomes `Direct(isEven_fid)` → LLVM inlines it. **2.54× measured** on `find`/`some`/
   `every` over 2 M Int32[] (`PERFORMANCE.md:294`).

### What cannot be devirtualized today

| Class | Reason |
|-------|--------|
| **Capturing closures** in non-combinators | Layer-1 admits them (relaxed gate); Layer-2 inlines only if captures are stack-resolvable. Recursive functions that pass closures across call frames (like the interp's `scan`/`parseExprLoop`) never reach the inline path — the closure escapes the immediate call site. |
| **Stored / parameter function values** | The monomorphizer sees `LocalGet{slot}` (a runtime value), not a concrete function. No SpecKey axis exists for "callback argument = whatever runtime value happens to be in slot X at call time." |
| **Arbitrary indirect calls** | A closure read from a container (`arr[i]`) or returned from another function — the callee is opaque until runtime. |
| **Callbacks to generic functions** | The callback's type contains an unresolved TypeVar; the devirt axis would need the outer generic instantiation first. |

`rename_inner_fns` (`monomorphize.rs:735-750`) exists specifically to prevent symbol
collisions when multiple SpecKey axes clone the same inner closures into different LLVM
symbols.

---

## 3. The interp hot loop in detail

**Source:** `benchmarks/compare/interp/interp.lin:1-189`.

The benchmark is "tokenize → recursive-descent parse → tree-walk eval" over 8 fixed
expressions × 10 000 repetitions (driver at lines 184-188).

### AST dispatch: unboxed SumNode

The `Ast = Num | BinOp` union is **sum-type-eligible** (every non-discriminant field is
scalar or a recursive `Ast` pointer). The representation pass assigns it `Packed(SumNode)`:
a heap struct `[u32 rc | u32 size | u64 desc | u32 tag | u32 pad | max-variant payload]`
(`crates/lin-ir/src/repr.rs:56-62`). The `evalNode` match dispatches via `SumTagEq` (a
single tag load + integer compare, `ir.rs:529-535`), not `lin_object_get`. The recursive
child reads `node["left"]` / `node["right"]` are **constant-offset pointer loads** from the
SumNode payload. Zero `lin_object_get` in the eval loop.

Similarly the parser's `kindAt` / `tokens[pos]["kind"]` reads from a `Token = {kind:String,
text:String}` sealed record — these are sealed scalar-ish records (String fields, not plain
scalars), so they DO go through `lin_sealed_get` / `lin_object_get` paths. `Token.kind` is
the hot discriminant string read (`kindAt` is called in every parser function).

### Hot call sites per iteration

Per `eval1` invocation (line 164-166):

1. **`tokenize` → `scan`** — tail-recursive scan over the string, calling `push(tokens, {…})`
   per token. Each `push` on a heap `Token[]` is a `lin_array_push` with a retain of the
   pushed `Token` sealed record.
2. **`parseFactor` / `parseTerm` / `parseTermLoop` / `parseExpr` / `parseExprLoop`** —
   recursive descent. Each level returns a **`Cursor = {node: Ast, pos: Int32}`** value.
   `Cursor` has a heap `node` field (the `Ast` SumNode pointer) and a scalar `pos`. It is a
   sealed record with one heap field → NOT all-scalar → **NOT stack-allocated** by the Stage-4
   escape pass (which gates on all-scalar, `escape.rs:83-88`). Each call frame allocs one
   `Cursor` on the heap.
3. **`evalNode`** — tree-recursive. Each call passes the `node: Ast` SumNode by value with a
   retain (the caller bumps the SumNode's RC before passing it down; the callee releases it on
   exit). Two child reads per `BinOp` node, each with a retain-on-escape.

The `PERFORMANCE.md §5` summary (lines 352-358) reports ~82 % object-allocation + RC, with
~1 000 retain/release sites per `eval1`. The alloc breakdown is approximately:
- One `Token[]` array per `tokenize` call (10-15 `Token` sealed records pushed into it).
- One `Cursor` sealed record per parser function call (~5-10 calls per expression).
- One `BinOp` SumNode per binary operator found.

### Why the dispatch is NOT the bottleneck

The `evalNode` match is `SumTagEq` — an O(1) integer compare in a handful of LLVM
instructions. The interp hot path has **no indirect calls** (`PERFORMANCE.md:283`). Named
calls are already `Direct`. Combinator calls (`.for`, `.map`) appear only in the outer
driver loop (lines 185-187), not in the per-expression hot path.

The confirmed bottleneck is **heap allocation + refcount traffic** for `Cursor` (one alloc per
parser frame) and `Token` (N allocs per `tokenize`), not call indirection.

---

## 4. Design options to close the gap

The PERFORMANCE.md §4 (lines 227, 352-358) and the sealed-struct-Cursor experiment (line
358: "regressed 9%") establish the following:

> The bottleneck is **per-frame heap allocation** (Cursor, Token) and the **RC traffic those
> allocations produce** (~1 000 retain/release per eval1), not call indirection or
> representation reads.

The options below are therefore ordered by their ability to eliminate allocations and RC,
not by call-ABI changes.

---

### Option A: Extend CallbackDevirt to capturing / stored closures

**Mechanism.** The Wave-C devirt axis (`monomorphize.rs:1808-1818`) is today gated on
"named, no-capture function." Relax it to any literal lambda whose free variables are all
module-level or stack-local (readable from the callsite's static scope). At the Layer-2
combinator lowerer, substitute the lambda body inline and drop the closure alloc.

**Why it won't help interp.** The interp's hot path contains **no combinator calls** in the
per-expression inner loop. The combinators appear only in the outer driver loop (lines
185-187: `exprs.for(e => …)`). The driver loop is ~0 % of runtime. Capturing-lambda devirt
does not touch `parseFactor`, `parseTermLoop`, `evalNode`, or any of the ~1 000
retain/release sites.

**Risk.** Medium. The existing Layer-1 gate already admits capturing lambdas conservatively;
Layer-2 is the gatekeeper. Extending the SpecKey axis to runtime closure values (rather than
named module-level functions) requires flow analysis to bound the lambda's capture set.

**Estimated interp speedup:** 0 %. Helpful for `array_pipeline`-shaped workloads only.

**Files:** `crates/lin-ir/src/monomorphize.rs` (CallbackDevirt axis),
`crates/lin-ir/src/lower/combinator.rs` (Layer-2 inline gate).

---

### Option B: Call-site specialization / inline cache

**Mechanism.** At each `CallTarget::Indirect` site, record the callee's function ID on first
call (a hidden monomorphic cache entry). On subsequent calls, check the cache ID; if it
matches, jump directly to the cached function's body without going through the boxed wrapper.
Analogous to a JavaScript JIT's inline cache but applied statically at compile time via the
monomorphizer.

**Why it won't help interp.** There are **zero `CallTarget::Indirect` sites** in the interp's
per-expression hot path. The confirmed bottleneck is object allocation + RC, not call
indirection. Path-8 Tier-3 (named-call devirt) was already confirmed dead-end because
"named calls are already direct" (`PERFORMANCE.md:283`).

**Risk.** High. Requires a new runtime dispatch path, a new IR instruction (or an annotation),
and soundness around mutation of the cache (thread safety, invalidation on closure escape).

**Estimated interp speedup:** 0 %. Helpful only if there were hot indirect calls in the interp,
which there are not.

**Files:** `crates/lin-ir/src/ir.rs`, `crates/lin-codegen/src/codegen/call.rs`.

---

### Option C: Unboxed calling convention for monomorphic scalar args

**Mechanism.** For direct calls (`CallTarget::Direct`) between Lin functions where all
parameters are scalar (Int32/Int64/Float64/Bool), today's ABI is already unboxed — codegen
emits `call i32 @fn(i32 %a, i32 %b)` directly (confirmed at `mod.rs:1148-1155`). The
performance cost is NOT in the call ABI for scalars but in the retain/release of heap-type
arguments (String, sealed records, arrays). A "borrow convention" for heap args — passing
a raw pointer without bumping the RC — would eliminate the retain before the call and the
release after it.

**What already exists.** `Convention{Borrow, Own, Inout}` is defined on `LinFunction`
(`ir.rs:294`, `ir.rs:647-657`) and `infer_conventions` (`ownership_verify.rs:662`) populates
it. The ownership verifier (`LIN_OWNERSHIP_SHADOW`) has been running shadow-clean since
Stage-1 (`PERFORMANCE.md:302`). **The infrastructure is in place but the lowerer does not yet
consume conventions to suppress retain at call sites.** Today, `lower/call.rs:304` calls
`retain_call_arg` unconditionally for all heap-typed arguments before a Direct call; the
matching release fires at scope exit. The `rc_elide` pass removes some same-block pairs but
cannot cross non-tail function boundaries.

**Mechanism for interp.** `parseFactor(tokens, pos)` is called from `parseTerm`. The
`tokens: Token[]` array is **not modified** and **does not escape** `parseFactor`'s frame —
it is read-only. `param_convention(0)` for `parseFactor` would be `Borrow`. If the lowerer
saw `Borrow` on a Direct call, it could skip the retain/release pair for that argument:
the caller already holds a valid reference that outlives the call, so no extra +1 is needed.

The same applies to `Cursor` fields passed through the parse chain and the `Ast` SumNode
passed to `evalNode`.

**Risk.** Medium. The convention inference is already live; the soundness condition (the
callee must never store or escape a Borrow parameter beyond its frame) is already checked by
the ownership verifier. The work is consuming `param_conventions` in `lower/call.rs` to
suppress `retain_call_arg` when the target parameter is `Borrow`. The `rc_elide` pass
already handles balanced pairs in the same block; this extends the elision to cross-call
pairs without post-dominance complications. Any unsound case that the verifier flags would
remain on the existing conservative `Own` path.

**Estimated interp speedup:** 20-40 %. Each elided retain/release pair removes 2 × (~5-10
LLVM instructions + a potential cache-miss on the RC word). With ~1 000 retain/release sites
per `eval1` and 80 000 `eval1` calls per run, ~160 M pairs are candidates. Even if half are
elided (Borrow-eligible direct-call params), 80 M fewer atomic-ish RC increments/decrements
is significant. Note: RC on these types is non-atomic (single-threaded), so each
retain/release is a load + add/sub + store — ~3-5 ns per pair on modern hardware.

This does NOT eliminate the allocation itself — it only removes the RC traffic on already-
allocated objects. Combined with Option D it would do more.

**Files:** `crates/lin-ir/src/lower/call.rs` (suppress `retain_call_arg` for `Borrow` params
on `Direct` calls), `crates/lin-ir/src/ownership_verify.rs` (already has the conventions,
no change needed there). `crates/lin-ir/src/lower/combinator.rs` possibly for the inner
callback arg. `call.rs` and `monomorphize.rs` in codegen are **free** (not touched).

---

### Option D: Stack-allocate non-escaping heap-field sealed records (Stage 4b)

**Mechanism.** The current Stage-4 escape pass (`crates/lin-ir/src/escape.rs`) is gated on
**all-scalar** sealed records only (`escape.rs:83-88`: "heap-field sealed records are NEVER
stack-allocated here — their stack drop would have to release heap fields — deferred").
`Cursor = {node: Ast, pos: Int32}` has one heap field (`node: Ast` SumNode pointer) and is
therefore excluded.

Extending Stage 4 to handle sealed records with a bounded number of owned heap fields
(SumNode pointers, String fields) would let non-escaping `Cursor` constructions be
stack-allocated with an **entry-block alloca** (like the all-scalar case at `mod.rs:1537`).
The stack drop would call `lin_sealed_release` on each heap field in turn — a fixed static
sequence known at compile time from the sealed descriptor (`types.rs:242-253`).

**Why this matters for interp.** Each `parseFactor` / `parseTerm` call constructs a `Cursor`
that is immediately consumed by its caller and does not outlive the call's stack frame (it is
returned but the caller uses and drops it in the same block). The escape analysis already
classifies these as non-escaping for the all-scalar gate; with the gate widened to include one-
heap-field records, those `Cursor` constructions would move off the heap entirely.

**Risk.** Medium. The stack-drop sequence for heap fields is straightforward (the descriptor
already lists them). The main hazard is aliasing: if the heap field itself (the `Ast`
SumNode) is retained by the callee, the stack shell must not be freed early. The carry-class
analysis in `escape.rs` already tracks this; the extension is additive. The prototype exists
conceptually from the Stage-4 architecture; only the `escape.rs:83-88` guard and the
`mod.rs:1537` gate check need widening.

**Estimated interp speedup:** 25-45 %. Eliminating `Cursor` heap allocs removes `lin_sealed_alloc`
calls and the corresponding RC on the node-field (no alloc → no retain-on-push → no
release-on-return). Combined with Option C (borrow conventions suppress RC on the SumNode
child reads), this could narrow the interp gap by 40-60 % combined.

**Files:** `crates/lin-ir/src/escape.rs` (widen the `is_stack_eligible_type` gate from
all-scalar to bounded-heap-fields), `crates/lin-ir/src/ir.rs` (no change needed — the
`stack: bool` flag on `MakeObject` already exists), `crates/lin-codegen/src/codegen/mod.rs`
line 1537 (widen the `sf.values().all(Self::is_sealed_scalar_field)` check to allow heap
fields and emit per-field static releases in the stack-drop path instead of a no-op).

**Active lane overlap:** `escape.rs` and `boxing.rs/data/intrinsics.rs/types.rs` are named
as active-lane files. **`escape.rs` is in the active zone.** Proceed with care; coordinate
with the reset-stage branches.

---

### Option E: Inline the hot runtime consumers (Tier-2/3)

**Mechanism.** Path-8 Tier-1 (bitcode runtime) bought <2 % because the consumer (the
closure call or `lin_tagged_arith` / `lin_object_get`) stayed opaque. Tier-2/3 means
*devirtualize the consumer first*, so LLVM can inline the callback body and see through the
box/unbox pair. Wave C (find/some/every devirt) is the realized form: it turned the indirect
per-element call into `@isEven(i32)`, LLVM inlined it, and box/unbox cancelled — 2.54×.

For interp, the "consumer" that stays opaque is **`lin_sealed_get`** (reading `Token.kind`
from a sealed record with String fields) and **`lin_array_get`** (reading `tokens[pos]`).
These are `CallTarget::Named` — already direct LLVM `call` instructions — but they call into
the runtime library which LLVM cannot inline (no bitcode bodies).

Specific inline opportunities in the interp path:
- `lin_array_get` on a `Token[]` array at a known-type index — could be emitted inline as a
  bounds-check + pointer load, similar to how `lin_string_byte_at` is inlined at
  `mod.rs:1157-1204`.
- `lin_sealed_get` on a `Token` sealed record for field `"kind"` — if the sealed descriptor
  is available at compile time (it is, for a statically-typed sealed record), the field offset
  is a compile-time constant and the call can be replaced with a constant-offset GEP + load.
  This is the "constant-offset field access" described in PERFORMANCE.md §4, line 170 — it
  already works for all-scalar sealed records; String-field records go through `lin_sealed_get`
  instead.

**Risk.** Low for `lin_array_get` (pattern already set by `lin_string_byte_at`). Medium for
`lin_sealed_get` on String-field records (need to emit inline null-check + load without
breaking the retain-on-read contract).

**Estimated interp speedup:** 5-15 % for `lin_array_get` inline; uncertain for `lin_sealed_get`.
The PERFORMANCE.md inline-cache result (path-2) showed that optimizing the "cheapest part" of
field access is rarely the bottleneck — but in the parser's `kindAt`, the sealed-record read
IS on the hot path (~3 calls per expression token).

**Files:** `crates/lin-codegen/src/codegen/mod.rs` (add inline cases for `lin_array_get` and
`lin_sealed_get` in the `CallTarget::Named` arm, analogous to the `lin_string_byte_at` case
at lines 1157-1204). `crates/lin-codegen/src/codegen/call.rs` and `monomorphize.rs` are
**free** (no changes needed there).

---

## 5. Concrete minimal first slice

### Recommended: Option C + Option D, in that order

These are the only options that attack the measured bottleneck (object allocation + RC). They
are independent of each other and can land in sequence.

#### Slice 1: Borrow-convention RC suppression at Direct call sites (Option C)

**What to build:**

1. In `crates/lin-ir/src/lower/call.rs`, find the `retain_call_arg` call at line 304 (in the
   global-function-slot branch) and the equivalent site in the imported-function branch.
   Before emitting the retain, check `callee_fn.param_convention(i)` — available after
   `infer_conventions` runs. If it returns `Convention::Borrow` and the argument is a
   `Direct` call to a concrete function (not an Indirect or Named runtime call), **skip the
   retain emission** and **suppress the matching scope-exit release** for that argument temp.
   The caller's own reference is valid for the callee's lifetime (convention soundness
   guarantees the callee does not extend it).
2. No changes needed in `rc_elide.rs`, `ownership_verify.rs`, or codegen. The convention
   table is already populated by `infer_conventions` before `rc_elide` runs (pipeline at
   `mod.rs:254`).

**Verification gate:** run `LIN_OWNERSHIP_SHADOW=1 lin build` on the full corpus. Zero
violations = safe to proceed. The ownership verifier already tracks borrow vs own for every
call site; a misclassified Borrow would surface as an `UnbalancedRetain` violation.

**File footprint:** `crates/lin-ir/src/lower/call.rs` only. All other files unchanged.

**Estimated interp speedup:** 15-30 % (RC traffic reduction; does not eliminate allocs).

**Risk:** Low. The Convention infrastructure and verifier are already live and shadow-clean.
The change is additive (wrong Borrow at any site degrades to a verifier violation, not a
silent UAF, because the convention was inferred conservatively — Own is the default).

#### Slice 2: Stack-allocate heap-field sealed records (Option D, Stage 4b)

**What to build:**

1. In `crates/lin-ir/src/escape.rs`, widen `is_stack_eligible_type` (currently only
   all-scalar sealed records) to include sealed records whose heap fields are exclusively
   SumNode pointers or String fields — a bounded, static-drop sequence. Keep the guard that
   excludes records with Array or nested-sealed fields (those require recursive drop walks
   that complicate the alloca model).
2. In `crates/lin-codegen/src/codegen/mod.rs` line 1537, widen the
   `sf.values().all(Self::is_sealed_scalar_field)` check. For the widened case (heap fields
   present), emit per-field static releases in the codegen cleanup path instead of
   the current no-op (`lin_sealed_release` is already a sentinel no-op for immortal-RC
   objects). Since heap-field stack records are not marked immortal, the cleanup must walk the
   field list and call `lin_rc_release` on each heap-field pointer at scope exit.
   Alternatively, assign the `SEALED_IMMORTAL_RC` sentinel and emit explicit per-field
   releases at each known drop point (as the ownership verifier already tracks them).

**Warning — active lane:** `escape.rs` is listed as an active zone (`lin-ir {repr,escape}.rs`
per the prompt). Coordinate with the reset branch before landing here to avoid conflict.

**File footprint:** `crates/lin-ir/src/escape.rs` + `crates/lin-codegen/src/codegen/mod.rs`.
`codegen/types.rs` may also need `is_sealed_scalar_field` widening.

**Estimated interp speedup:** 20-35 % additional (alloc elimination for Cursor). Combined
with Slice 1: 35-60 % total interp improvement expected, narrowing the gap from ~30× Rust
to ~12-18× Rust. Closing to ~8× would require multi-value returns or arena allocation,
which are multi-week projects.

**Risk:** Medium. The escape analysis carry-class machinery is sound for this extension; the
main new surface is the per-field static drop in codegen. A regression test over the interp
benchmark (`cargo run -p lin -- test benchmarks/compare/interp/`) and the full integration
suite is sufficient to gate landing.

---

## Summary table

| Option | Attacks | Interp speedup | Risk | Files (free vs active) |
|--------|---------|---------------|------|------------------------|
| A — broaden devirt to capturing closures | Combinator indirect calls | 0 % (no hot combinators) | Medium | monomorphize.rs (FREE) |
| B — call-site inline cache | Indirect call overhead | 0 % (no hot indirect calls) | High | call.rs (FREE) |
| **C — Borrow convention RC suppression** | **RC traffic at Direct calls** | **15-30 %** | **Low** | **lower/call.rs (FREE)** |
| **D — Stack-allocate heap-field sealed records** | **Heap allocs for Cursor/Token** | **20-35 %** (additive to C) | **Medium** | **escape.rs (ACTIVE LANE ⚠), codegen/mod.rs** |
| E — Inline `lin_array_get` / `lin_sealed_get` | Named runtime call overhead | 5-15 % | Low-Medium | codegen/mod.rs (FREE) |

**Recommended first slice:** Option C (Slice 1), then Option D (Slice 2).  
**File footprint:** `crates/lin-ir/src/lower/call.rs` (Slice 1, free); `crates/lin-ir/src/escape.rs` + `crates/lin-codegen/src/codegen/mod.rs` (Slice 2, escape.rs is active-lane).  
**Estimated total interp speedup:** 35-60 % combined (363 ms → ~145-235 ms), narrowing the Lin/Rust gap from ~30× to ~12-18×. Closing to ~5× (Go range) requires alloc elimination via multi-value returns or arena scopes — a separate, higher-risk project per PERFORMANCE.md line 356.  
**Risk:** Low for Slice 1 (ownership verifier gates safety); Medium for Slice 2 (escape.rs lane overlap, new static-drop codegen path).
