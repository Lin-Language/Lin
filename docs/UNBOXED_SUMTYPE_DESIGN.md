# Unboxed Tagged Sum-Type Representation — Design + Scope Spike

**Status**: Stages 0–4 are now LIVE (on branch `feat/sumtype-integration`, pending merge). Stage 2
packs RECURSIVE sum types: a `type Ast = Num | BinOp` whose `BinOp` carries recursive
`left`/`right : Ast` children compiles end-to-end as unboxed `SumNode`s with the recursive children
stored as 8-byte owned `*SumNode` pointer slots (`KIND_SUMNODE` in the static `SumDesc`), a
recursive drop walk, const-offset child-load (borrowed interior `*SumNode`), and nested-literal
discriminant pushdown so a nested `{ "kind": … }` literal constructs a child `SumNode` directly. The
recursive RC drop is ASan-clean (every node freed exactly once; only the immortal string-interner
allocations "leak", identically to the boxed baseline).

**Stage 3 (keep-packed across containers) — DONE, via runtime-tag dispatch.** A `SumNode` stored into
a boxed RECORD field or a `{String:_}` MAP value stays packed-by-pointer (no materialize): a distinct
runtime tag `TAG_SUMNODE` (lin-common/tags.rs) marks the slot, so the read-back dispatches on the tag
(`TAG_SUMNODE` → unwrap the packed node zero-copy + retain; `TAG_OBJECT` → project). This resolved the
store-sees-sum-union vs read-sees-partial-`Named` repr ASYMMETRY *without* the general repr STEP-4
coercion pass — the tag makes the slot self-describing. Reads into union/Json position, and the
genuinely-dynamic consumers (`toString`/`==`/json/spread/worker-transfer), still MATERIALIZE to a real
`LinObject` (a per-type materializer fn-ptr stored at the `SumDesc` head drives the runtime walkers and
the thread-transfer deep-copy), so a kept-packed pointer never escapes its representation domain.

**Stage 4 (interp port) — DONE, the payoff gate PASSED.** `benchmarks/compare/interp/interp.lin` AST
ported to `type Ast = Num | BinOp` (operator as an `Int32` code). Measured (this machine, release,
medians): Json-AST baseline **0.526 s** → unboxed-but-materializing-cursor **0.768 s (a regression —
the cursor round-trip swamped the eval win, exactly the §8 prediction)** → **keep-packed 0.437 s
(1.20× faster than Json, 1.76× faster than the materializing port)**. `evalNode` is fully unboxed (0
`lin_object_get`, inline tag-switch). RESULT=10460000 unchanged. With the AST unboxed, the remaining
floor is the tokenizer strings + `Token[]` allocation + the `range().for()` closure ABI (≈10× still to
Node), NOT the AST — so Stage 5 (FBIP node reuse) is deferred as low-value for this workload until
profiling says node alloc/RC dominates (it no longer does).

Stage 2 self-recursion is detected ENV-FREE: a recursive child survives type resolution as
`Type::Named(self_name)` (the checker leaves the cyclic back-reference unexpanded), and a sum
union's unique self-name is the single `Named` appearing in its variant fields
(`Codegen::sum_recursive_self_name` / `repr::sum_recursive_self_name`, kept byte-identical). A
union with >1 distinct `Named` (mutual recursion) or any other heap/union field stays boxed
(fail-safe) — mutual recursion is out of Stage-2 scope. A recursive sum value still MATERIALIZES to
a boxed `LinObject` at a Json/union/generic/cross-module boundary (the Stage-1 boundary behavior);
Stage 3 makes that keep-packed.

## Stage 1 — LIVE (the make-it-live milestone, branch `feat/sumtype-live`)

The seed in `lin-ir::repr` is ON: a Stage-1-eligible sum type (`type T = A | B`, ≥2 sealed-record
variants sharing a distinct `StrLit` discriminant, every OTHER field an unboxed scalar, NO
recursive/heap/union fields) packs end-to-end as an unboxed `SumNode`. **The CALL ABI rule:**

- A sum value's repr is `Packed(SumNode)` at every definite-packed site: a construction literal, a
  sum-typed function PARAM, a sum-RETURNING call, a `match`-narrowed temp, a bare local read.
- A sum-typed PARAM receives a `SumNode` pointer; a sum-RETURNING function returns a `SumNode`
  pointer (we own the Lin calling convention). A same-type call/return is a verbatim pointer carry
  (no coercion). The recursive self-call passes the pointer through (a `Named` param resolving to the
  sum type is a pass-through, mirroring sealed records).
- At every BOXED/dynamic boundary the node MATERIALIZES once to a boxed `LinObject`: a `Json`/union/
  generic param or return, `toString`/`==`/spread, an array element store, a map value store. The
  read-back out of a boxed container PROJECTS the boxed value into a fresh `SumNode`, so the repr
  stays consistent (a `SumNode` everywhere the type says sum) — which the load-bearing `repr::verify`
  proves across the whole corpus + test suite with the seed ON.
- DISPATCH is O(1): `match s is Circle` over a `SumNode` lowers to `SumTagEq` (an inline-tag load +
  integer compare), NOT `lin_matches_schema`/`object_get`. A narrowed scalar field read is a
  constant-offset load. A construction packs directly via `sumnode_construct` (no boxed round-trip).

**Mechanism**: the boundary coercions are threaded on the OPERAND's repr (`compile_ir_coerce_with_repr`,
the container store/read paths, the `==` and RC ops in codegen all read `func.repr`), never the
static `Type` — because a sum `Type` is `is_sum_type` true even while a particular occurrence is
physically boxed.

**Measured**: a dispatch-heavy microbench (construct-once + tight `match`+field-read loop, 8M iters)
is **~4.5× faster packed** than the boxed `Json`/`has`-pattern equivalent (2.98s vs 13.46s, median of
9 interleaved). ASan-clean (no UAF/double-free/overflow; every sum value freed exactly once; only the
pre-existing immortal string-interner allocations leak, identically to the boxed baseline).
`repr::verify` is load-bearing-green with the seed ON.

**Falls back to boxed (still sound)**: any type outside the strict gate (recursive/heap/union/Named
variant fields → a boxed union, Stage 2+); a sum value flowing through a `Json`/generic/cross-module
boundary materializes (then is boxed thereafter unless re-read into a sum slot, which re-projects).

---

**Original spike status (below)**: SPIKE / design only. This document was the deliverable of a
reconnaissance + design spike on branch `spike/unboxed-sumtype` (base master `19367a3`).

**One-line verdict (full reasoning in §8): BUILD IT, but as a long multi-stage milestone, and only
after the two cheap checker prerequisites land first.** A throwaway PoC (§9) measured that an unboxed
tagged tree-walk is ~30× faster than the current boxed-`Json` AST on the interp benchmark's eval
phase, and the parse phase (which also builds boxed AST/cursor nodes) is the other ~90% of interp's
time — so the payoff is large and it is the *only* representation that closes the gap (sealed records
and union-discrimination both measured no-ops for this workload — see §0). The risk is that it is a
genuinely large piece of work (the staged plan in §7 is ~6 landable stages) touching checker, IR,
repr pass, codegen, and runtime, all RC/ASan-sensitive.

---

## 0. Why this, and why nothing cheaper works

The interp benchmark (`benchmarks/compare/interp/interp.lin`) types its AST as `:Json`
(`evalNode = (node: Json): Int32`, `interp.lin:125`), so every node is a boxed string-keyed
`LinObject` (`crates/lin-runtime/src/object.rs:18`) reached by a non-inlined `lin_object_get` per
field (`object.rs:530`). A just-completed spike PROVED that rewriting interp with a concrete typed
union (`type Ast = Num | BinOp`) is a measured no-op, because the AST is intrinsically:

1. **a UNION** — `Num | BinOp` has no single packed layout. The sealed-record packer
   (`Codegen::sealed_fields`, `crates/lin-codegen/src/codegen/types.rs:223`) is *monomorphic*: it
   only fires on one `Object { sealed: true }` field map. A union of two shapes fails the gate and
   stays boxed.
2. **recursive** — `BinOp.left/right : Ast`. A self-recursive field survives type resolution as
   `Type::Named` (not an inlined `Type::Object`), and `sealed_field_kind` (`types.rs:190`) returns
   `None` for `Named` → the whole record is kept boxed (the gate's own termination argument,
   `types.rs:218-222`; mirrored in `lin_ir::lower::is_sealed_field_ty`, `lower.rs:1309`).

So neither sealed records nor the shipped union-discrimination tag-compare help — the value stays a
boxed `LinObject`. The only thing that closes it is a representation Lin lacks today: an **unboxed
tagged sum type** — an inline discriminant + a max-variant-sized packed payload, with recursive
children stored *by pointer to the same repr*, constant-offset field access, and an O(1) tag-switch
dispatch (no `lin_matches_schema`, no string compare, no hash lookup). This is exactly what
ML/OCaml/Rust/Roc/MoonBit do for ADTs and why their tree-walkers are fast. Crucially, it **composes
with the existing repr-inference pass** (ADR-062): it is another `Repr` variant, decided by the same
single-owner dataflow analysis, with the same materialize-at-dynamic-edges boundary discipline.

---

## 1. The representation

### 1.1 Source surface (no new syntax needed)

A sum type is already spellable as a union of named/sealed records:

```lin
type Num   = { "kind": "num",   "value": Int32 }
type BinOp = { "kind": "op",    "op": String, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
```

The *discriminant* is a field shared by all variants whose value is a distinct `StrLit` per variant
(`"kind"` here) — the same StrLit-discriminant the shipped union-discrimination work keys on. No
syntax change is required for stage 1; an optional sugar (`type Ast = Num(Int32) | BinOp(...)`) can
come later. The checker work in §6 is about *recognising* this shape, not parsing new tokens.

### 1.2 Physical layout — `SumNode`

An unboxed sum-type value is a pointer to a heap `SumNode`, laid out as a packed struct mirroring the
sealed-record header (`crates/lin-runtime/src/sealed.rs:8`) so the existing RC primitive
(`lin_rc_retain` bumping the u32 at offset 0) works unchanged:

```text
[ u32 refcount | u32 size | u64 desc_ptr | u32 tag | u32 _pad | <payload, max-variant-sized> ]
   @0            @4         @8             @16        @20        @24...
```

- **offset 0 (`refcount`)** — identical to sealed records, so `lin_rc_retain` / the IMMORTAL_RC
  stack-sentinel (`sealed.rs:155`) work verbatim.
- **offset 4 (`size`)** — total byte size = `24 + max_variant_payload`. Lets a child be released
  without the caller knowing its variant (`lin_sealed_release_self`, `sealed.rs:241`).
- **offset 8 (`desc_ptr`)** — pointer to a static **`SumDesc`** (§1.3), the soundness mechanism for
  the recursive drop walk — analogous to the sealed `SealedDesc` (`sealed.rs:29`).
- **offset 16 (`tag`)** — the inline discriminant: a small dense integer (0,1,2…), one per variant,
  assigned in declaration order. This is the switch key (§2). Stored inline, NOT in a `TaggedVal`.
- **offset 24 (payload)** — the variant's fields packed exactly like a sealed record's field block
  (`sealed_field_layout`, `types.rs:255`), sized to the **max over all variants** so every variant
  fits in one fixed-size node (a tagged *union* in the C sense). Recursive `Ast` fields are stored as
  an 8-byte owned pointer slot to another `SumNode` (§1.4).

**Why max-variant-sized inline, not per-variant alloc.** A fixed node size means an arena/freelist
can recycle nodes (FBIP territory) and avoids a second indirection. The waste (a `Num` node carrying
`BinOp`'s slot space) is small for typical ASTs (2–4 variants). An alternative — per-variant exact
size — is also viable (each node's `size@4` is exact) and saves memory at the cost of giving up
uniform reuse; this is a tuning decision deferrable to a later stage. **Recommendation: start
max-sized for layout simplicity, revisit if memory matters.**

### 1.3 The tag, and reusing `TaggedVal`

The 16-byte `TaggedVal` (`crates/lin-runtime/src/tagged.rs:41`, `{u8 tag, [7]pad, u64 payload}`) is
**NOT** reusable as the sum-node itself: its tag space is the fixed runtime type-tag set
(`TAG_OBJECT`, `TAG_ARRAY`, … in `lin-common/src/tags`), it has no refcount slot, no descriptor, and
only one 8-byte payload word — it cannot hold a multi-field variant inline. A sum node needs a
*per-sum-type* discriminant (which variant) and a *multi-word* payload. So the sum node is its own
struct (§1.2). However:

- A sum-type value, when it crosses a dynamic boundary (stored in a `Json`/`Map`/union slot, passed
  to a generic closure callback, sent to `toString`), IS boxed as `TaggedVal(TAG_OBJECT, ptr)` —
  reusing the *existing* box machinery. This is the **materialize-to-boxed boundary** (§4), exactly
  how sealed records cross dynamic edges today (`compile_ir_coerce`, `match.rs:173-186`).
- The inline `tag` is a `u32` (not a `u8`) only for alignment of the following payload; a `u8` plus
  padding is equivalent. Keep it `u32` to match the natural 8-byte field alignment the sealed layout
  already assumes.

### 1.4 Recursive children — boxed-by-pointer-but-pointee-is-unboxed

`BinOp.left : Ast` cannot be inline-by-value (infinite size). It is an **8-byte owned pointer slot to
another `SumNode`**. This is *not* a boxed `TaggedVal` — it is a direct `SumNode*`. This is precisely
analogous to the repr pass's `Inner::WrapsPacked(Layout)` state (`repr.rs:63-71`): "a slot whose
payload pointer is a STILL-PACKED buffer", except here the pointee is a `SumNode` rather than a
sealed struct. The descriptor (§1.5) records these child slots as a new `KIND_SUMNODE` so drop
recurses into them. Field access (§3) of a recursive field is a single pointer load yielding another
unboxed `SumNode*` — the inner `evalNode(node["left"])` reads `node`'s `left` slot directly and
re-enters the tag switch, **no unboxing, no `lin_object_get`**.

### 1.5 `SumDesc` (static, one per sum type)

Like `SealedDesc` (`sealed.rs:29`), but per *variant*, because heap-field offsets differ by variant:

```text
SumDesc = [ u32 variant_count | VariantDesc * variant_count ]
VariantDesc = [ u32 heap_field_count | { u32 byte_offset, u32 kind } * heap_field_count ]
```

`kind` extends the sealed `KIND_*` set (`sealed.rs:56-63`) with `KIND_SUMNODE = 4` (a recursive
child → `lin_sumnode_release_self`). Drop reads `tag@16`, indexes into `SumDesc` to get that
variant's heap-field list, releases each, then frees. (For scalar-only variants the per-variant list
is empty — a pure refcount decrement + free.)

---

## 2. Discrimination (match / is → tag switch)

Today `is <Object>` runs the recursive `lin_matches_schema` structural walker
(`compile_ir_matches_schema`, `match.rs:83`), and `is <Union>` ORs together runtime type-tag
compares (`compile_ir_is_type`, `match.rs:38-49`). Neither is O(1) for a sum type.

For an unboxed sum-type value, `match x is Num => … is BinOp => …` lowers to:

1. Load `tag@16` from the `SumNode*` (one inline load — already unboxed, no `lin_get_tag` call, no
   schema walk).
2. An LLVM `switch` on that `u32` to the arm blocks (O(1), jump-table-able by LLVM).

This **reuses and extends the shipped StrLit-discriminant work**: the checker already recognises that
a union's members are discriminated by a shared `"kind": StrLit` field; that analysis assigns each
member a stable tag (which becomes the inline `tag@16`), and the codegen `match`/`is` site, when the
scrutinee's repr is `Repr::SumType(layout)` (§4), emits the inline-tag switch instead of
`compile_ir_matches_schema`. A scrutinee that is *boxed* (came from a `Json` edge) first cheaply
unboxes the `SumNode*` from its `TaggedVal(TAG_OBJECT)` then loads the tag — still O(1).

**Exhaustiveness** is already computed by the checker over union members; the switch's default arm is
`unreachable` when exhaustive, an error/fallthrough otherwise — no new exhaustiveness logic, it
rides the existing union exhaustiveness pass.

---

## 3. Field access (constant-offset within a narrowed variant)

After `match x is BinOp => x["left"]`, inside that arm `x` is narrowed to `BinOp`, whose layout is a
fixed field map → `x["left"]` is a constant-offset load at `24 + sealed_field_layout(BinOp.fields,
"left")` (`types.rs:255`), exactly like a sealed-record `FieldGet`. A recursive `left` field load
yields a `SumNode*` typed `Ast`, carrying `Repr::SumType` again — so a chained
`evalNode(node["left"])` is: const-offset pointer load → recurse → tag switch. No `lin_object_get`,
no hash lookup.

The narrowing must expose the variant's packed layout to the FieldGet site. The checker already
narrows a union scrutinee to the matched member inside a match arm; the repr pass then sees the
narrowed temp's type is the concrete variant record and assigns it the variant's packed layout
(§4). This is the same mechanism by which sealed-record FieldGet gets its `PackedStruct` layout
today (`repr.rs:427-433`).

---

## 4. Where it slots into the repr pass (ADR-062)

A new lattice variant in `crates/lin-ir/src/repr.rs`:

```rust
pub enum Layout {
    PackedStruct { fields: IndexMap<String, Type> },          // existing
    PackedSealedArray { elem_layout: ..., on_heap: bool },    // existing
    SumNode { variants: Vec<(u32 /*tag*/, IndexMap<String, Type>)> }, // NEW
}
```

and `Repr::Packed(Layout::SumNode { .. })` is the unboxed-sum-type repr. (No new top-level `Repr`
variant is needed — it is a `Packed` layout, which means it automatically inherits the existing
boundary catalogue: islands stay packed, dynamic edges materialize.)

- **Decide site**: a `MakeObject` whose `ty` is a *member of a recognised sum type* (the checker tags
  it) and whose discriminant field is a known StrLit → produce a `SumNode` of that sum type's layout
  (analogous to `make_object_repr`, `repr.rs:275`). A `match`-narrowed temp typed as a variant gets
  the SumNode layout for that variant.
- **Carry**: a `SumNode` value flows through Copy/Bind/Phi/recursive-field-load preserving its repr
  via the existing carry classes (`carry.rs`), exactly as `PackedStruct` does.
- **Boundaries** (the same four ADR-062 catalogues):
  - **container store** (sum value into `Json`/`Map`/union slot) → `BoxKeepPacked` to
    `TaggedVal(TAG_OBJECT, SumNode*)` — O(1), keep-packed-by-pointer (the existing Stage-4 op,
    `repr.rs:484`).
  - **dynamic consumer** (`toString`/`keys`/spread/`==`/FFI/generic boxed `for`) → materialize the
    `SumNode` into a boxed `LinObject` once (a new `sumnode_materialize_to_object`, mirroring
    `sealed_materialize_to_object`, `match.rs:177`).
  - **union membership** → box as above.
  - **cross-representation call arg** → coerce (project a boxed/Json arg into a fresh `SumNode`,
    mirroring `sealed_project_from`, `match.rs:170`).

The **oracle** (`oracle_check`, `repr.rs:540`) and **verifier** (`verify`, `repr.rs:717`) get
`SumNode` arms asserting every match-switch / FieldGet / child-load reads a `SumNode` operand whose
repr is the matching `SumNode` layout — making a sum/boxed mismatch a debug-build compile panic, the
same soundness gate that protects sealed records.

---

## 5. RC / ownership

A `SumNode` is reference-counted exactly like a sealed record:

- **Construct**: each heap/child field retained once; the node owns +1 of each child. (Codegen emits
  the retains, mirroring sealed-record construction, `sealed.rs:41`.)
- **Drop at rc==0** (`lin_sumnode_release` / `_self`): read `tag@16`, index `SumDesc` for that
  variant's heap-field list, release each (a `KIND_SUMNODE` child recurses via the child's *own*
  tag+desc), then free. This is the sealed-record `release_heap_fields` walk (`sealed.rs:116`)
  generalised to be variant-indexed. The tree drop walks the whole tree releasing children — the
  standard ADT drop.
- **Retain/clone**: bump the u32 (shared) or deep-copy per descriptor for thread transfer
  (`clone_sealed` analogue). The IMMORTAL_RC stack sentinel (`sealed.rs:155`) carries over for
  escape-analysed stack/arena nodes.

This is the recurring UAF/double-free bug class (memory: RC/ownership invariants), so **every stage
is ASan-gated** (§7). The borrowed-vs-owned contract for a child-field load is the same as a
nested-sealed-field load: `FieldGet` of a child yields a *borrowed* interior pointer (the parent
still owns it); the lowerer must retain if it escapes (this is exactly the `is_rc_type` /
record_escape_alias discipline already in `lower.rs`).

---

## 6. Checker prerequisites (the 4 interp-spike gaps)

The interp spike found 4 gaps. Their dependency status for this project:

1. **Nested-literal discriminant pushdown** (a nested `{ "kind": "op", ... }` literal in
   `left`/`right` position must be checked against the expected variant so it constructs a `SumNode`,
   not a fresh anonymous boxed object). **PREREQUISITE** — without it the recursive children are
   boxed and the whole tree degrades. This is an extension of the existing expected-type pushdown
   that `infer_call`/`infer_dot_call` already do for array-literal args (ADR-062 Consequences,
   "producer/consumer literal drift — FIXED"). Medium effort.

2. **`has`/`is` patterns covering union variants for exhaustiveness.** The checker must accept
   `match x is Num / is BinOp` (or `has "value"` / `has "left"`) as exhaustive over `Ast`'s members.
   **PREREQUISITE for ergonomic match**, but the exhaustiveness machinery exists for unions already;
   this is wiring StrLit-discriminant variants into it. Medium effort. Can be developed independently
   of the repr (it is pure type-checking; a boxed sum type still type-checks, just runs slow).

3. **Indexing a `Type::Named` result of a mutually-recursive call.** `infer_index`
   (`crates/lin-check/src/checker/expr.rs:484`) has **no `Type::Named` arm** — it falls straight to
   the `_ => Err("Cannot index into type {}")` arm. A mutually-recursive parser returning `Ast`
   (a `Named`) then indexed (`result["node"]`) is a hard type error today. **PREREQUISITE** — must
   add a `Type::Named` arm that resolves the alias one level (via the type env, like
   `compat`/`Named` unfolding) and re-runs the index. **Low effort, independently landable, and
   useful on its own** (it's a real ergonomic hole). Recommend doing this FIRST.

4. (Implied 4th) **Declaring the sum type ergonomically** — recognising `type Ast = A | B` where each
   member is a record with a shared StrLit discriminant as a *tagged sum* the repr can pack.
   **This is the core checker work** that feeds the repr pass (§4); it is the actual feature, not a
   prerequisite.

**Recommendation: gaps 2 and 3 are cheap, independently landable, useful on their own, and should
land FIRST as a no-risk warm-up (they only improve type-checking; no repr/codegen/RC change).** Gap 1
and the sum-type recognition are part of the feature proper.

---

## 7. Staged plan (independently landable, ASan-gated)

Each stage: lands on its own, passes `cargo test --workspace` + the formatter corpus gate, and is
ASan-clean (RC is the dominant risk — `cargo test` does NOT catch UAF; only ASan does — memory:
RC/ownership invariants).

| Stage | Scope | Effort | Risk | ASan focus |
|---|---|---|---|---|
| **0** | Checker warm-up: add `Type::Named` arm to `infer_index` (gap 3); accept StrLit-discriminant union members in match exhaustiveness (gap 2). No repr/codegen change. | S | Low | n/a (type-check only) |
| **1** | **Non-recursive, 2-variant, scalar-only sum type packs.** `type T = A\|B`, each a sealed scalar record with a shared StrLit discriminant, NO recursive fields. Runtime `SumNode` alloc + `SumDesc`; repr `Layout::SumNode`; `MakeObject` decide site; `match is` → inline-tag switch; const-offset FieldGet; materialize-to-boxed at every dynamic edge. Oracle+verify arms. | L | Med | construct/drop/match/field-read/box-at-edge of a non-recursive sum value |
| **2** — **LIVE** | **Recursive sum type.** Added `KIND_SUMNODE` child slots + the recursive drop walk + child-field load returning an unboxed `SumNode*` (borrowed interior). This is the interp `Ast`. Nested-literal discriminant pushdown (gap 1). Self-recursion detected env-free via the union's unique `Named` self-name. | L | **High** | the tree drop walk (recursive release), child-load borrow, no double-free on the subtree — ASan-clean (every node freed once; only immortal interner leaks) |
| **3** | **Keep-packed across `Json`/`Map`/union slots** via `BoxKeepPacked`/`UnboxKeepPacked` reuse (so a parser returning `Ast` stored in a cursor object stays unboxed by-pointer). | M | Med | box-keep-packed round-trip; the keep-packed `TaggedVal` release path |
| **4** | **Interp rewritten + measured.** Port `interp.lin` to `type Ast = Num\|BinOp`; confirm the eval AND parse phases unbox; measure vs Rust/Node. Add a corpus regression fixture + an ASan lifecycle test. | M | Low | full interp lifecycle under ASan |
| **5** (opt) | **Arena/FBIP node reuse** (recycle fixed-size `SumNode`s) and/or per-variant exact sizing. Pure perf tuning. | M | Med | reuse-after-free correctness |

Stages 0 is a no-risk prerequisite. Stages 1→2 are the hard core (recursive RC). Stage 4 is the
payoff gate — **do not commit to 5 until 4 confirms the measured win.**

---

## 8. Honest payoff estimate + build/don't-build verdict

### Measured bounds (PoC, §9, this machine, release builds)

| | time | vs Rust |
|---|---|---|
| Rust reference | ~15 ms | 1.0× |
| Node reference | ~50 ms | 3.3× |
| **Lin interp today (boxed `Json` AST)** | **~1050 ms** | **70×** |
| Lin boxed-AST eval-only (80k tree-walks, parse once) | ~90 ms | 6× |
| **Lin UNBOXED flat-arena eval-only (same 80k walks)** | **~3 ms** | **0.2×** |

Two findings:

1. **The eval phase is ~30× faster unboxed** (90 ms → 3 ms) — and an unboxed flat tree-walk actually
   *beats* Rust on eval alone (Rust's 15 ms includes its parse). This validates that unboxing the AST
   approaches/exceeds Go/Node on the tree-walk, which is the whole premise of the project.
2. **Eval is only ~90 ms of interp's ~1050 ms; the other ~960 ms is the parse/tokenize phase**, which
   *also* builds boxed objects (the `{ "node": …, "pos": … }` cursors and the `{ "kind", "op",
   "left", "right" }` AST nodes — `interp.lin:72-118`). The sum-type repr unboxes the AST-node
   construction in the parser too, and the cursor is a 2-field record that sealed-records/SumNode can
   pack. So the representation change attacks both phases.

### Realistic interp landing

If both the eval AST and the parser's node construction unbox, interp should drop from ~1050 ms into
roughly the **~50–150 ms** range — i.e. **Node-competitive (within ~1–3×), and ~7–20× faster than
today.** It will likely NOT reach Rust's 15 ms, because:

- **The next bottleneck after the AST is string handling**: the tokenizer does `charCode`/`substring`
  per char (`interp.lin:42-56`), and `charCode` is O(n)/call (memory: charCode O(n²) → byteAt; the
  `byteAt` fix exists unmerged on `perf/string-charcode`). The token `"text"` is still a boxed
  `LinString`. So strings, not the AST, become the floor.
- **Closure-callback boxing** at the `.for(...)` driver boundary (the per-element `TaggedVal*` ABI)
  persists unless the callback is specialised (ADR-044 territory).

### Is the *workload class* worth it?

This is the real question. interp is a deliberately-chosen *representative* of a class, not an
artificial micro-stressor: **recursive-sum-type-heavy code = interpreters, parsers, compilers, tree
algorithms, AST/IR transforms, JSON/structured-data processors, expression evaluators.** Lin's own
self-hosting ambitions (a Lin compiler written in Lin) are *exactly* this class — the lexer/parser/
checker are sum-type tree code. And Lin already positions itself around "strict JSON data… pattern
matching" (CLAUDE.md), so recursive tagged data is core, not peripheral. This is not a narrow benefit.

The counter-argument: it is a **large** project (6 stages, deep RC/ASan risk in stage 2), and the
*cheapest* alternative wins (sealed records, union-discrimination) were already measured to do
nothing here (§0), so there is no incremental shortcut — it is all-or-most-of-the-way or nothing.

### Verdict: **BUILD IT** — staged, prerequisites first.

Reasoning: (1) the measured payoff is large (~7–20× on interp, 30× on the eval kernel, validated by
PoC); (2) it is the *only* representation that closes the recursive-data gap, and that gap is a core
workload class for Lin (interpreters/parsers/compilers, including self-hosting); (3) it composes
cleanly with the existing repr pass and reuses the entire sealed-record layout/RC/descriptor
machinery and the shipped StrLit-discrimination — it is "another `Packed` layout", not a new
universe; (4) it is decomposable into independently-landable, individually-valuable stages, so it can
be paused/measured at stage 4 before over-investing. The honest caveat: **stage 2 (recursive RC drop)
is the high-risk piece and must be ASan-gated hard**, and the realistic ceiling is Node-competitive,
not Rust-competitive, because strings/closure-ABI become the next floor.

**If only one thing is done:** land Stage 0 (the two cheap checker fixes) regardless — they are
no-risk ergonomic improvements (`infer_index` `Named` arm + union-variant exhaustiveness) that are
useful independent of this whole project.

---

## 9. PoC measurement (throwaway, done in this spike)

Built two throwaway `.lin` programs with `lin build` (release compiler + release runtime staticlib)
and timed them against the real interp and the Rust/Node references:

- **`poc_interp.lin`** — the same 8 interp expressions, hand-encoded as flat `Int32[]` arenas (node =
  `[kind, op, left, right]`, kinds `0=num 1=binop`), evaluated by an `evalNode(arena, idx)` that does
  constant-offset reads (`arena[base]`, `arena[base+2]`…) + a tag switch (`if kind == 0 … else …`),
  80k walks (8 exprs × 10000 reps). This *simulates* what an unboxed `SumNode` tree-walk's hot path
  would compile to: no boxed objects, no string keys, no `lin_object_get` — inline scalar loads + a
  branch on the discriminant. **Result: ~3 ms, RESULT=10460000 (correct).**
- **`interp_evalonly.lin`** — the real interp with parsing hoisted out of the REPS loop (parse each
  expr once into a `val`, eval 10000×), isolating the boxed-`Json` AST eval cost. **Result: ~90 ms.**

The 30× eval-phase delta (90 ms boxed → 3 ms unboxed) and the observation that ~960 ms of the full
~1050 ms interp is the (also-boxed) parse phase are the basis for §8's estimate. All PoC artifacts
were removed after measurement; the numbers are reproducible by regenerating the arenas from the 8
expressions and `lin build`-ing the flat-arena walker.

*(Caveat: the flat-arena PoC under-models the real `SumNode` by using one shared arena array and
integer indices instead of pointer children + per-node refcounting, so it omits the RC traffic a real
`SumNode` tree drop incurs. It therefore bounds the eval *compute* cost, not the alloc/RC cost — but
the alloc/RC cost is paid once per tree (build/drop), amortized over the inner reads, and is the same
order as sealed-record construction which already measures fast. The eval-only 30× and the
parse-phase share are the load-bearing numbers.)*
