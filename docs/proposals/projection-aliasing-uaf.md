# Bug + design decision: `val x = obj[k]` dangles when the container grows (interior-pointer UAF)

Status: CONFIRMED bug on master (memory-safety), needs a language-owner design decision before a fix.
Found while typing RAPTOR's `kConnections`; root-caused to a general, typed-map-INDEPENDENT issue.

## The bug (verified on clean master)

Binding a `Json`/union value projected out of an object (or array) and then **mutating the container
in a way that grows it** leaves the binding dangling — a use-after-free:

```lin
var results: Json = {}
results["C"] = []
val bC = results["C"]        // binds an INTERIOR POINTER into results' entries buffer
results["B"] = []            // grows results -> migrate_inline_to_heap / realloc -> entries buffer MOVES -> bC dangles
results["A"] = []            // grows again
bC.for(n => null)            // heap churn: closure-box allocs reuse the freed old entries block
push(bC, {"label":"C"})      // writes through the stale pointer -> crash / corruption
```
Crashes `null pointer dereference at crates/lin-runtime/src/array.rs:350`, exit 134.

## Root cause — NOT what it first looked like

The first investigation blamed `for` for over-releasing a borrowed receiver. **That is wrong** — verified
at the IR level: `bC.for(...)` lowers to `std_iter_for(iterable, closure)`, which retains/releases ONLY
the closure and never touches the iterable's refcount. There is no over-release anywhere in the program.

The real cause: `lin_object_get` (`crates/lin-runtime/src/object.rs:517`) returns an **interior pointer**
`&entry.value` into the object's `entries` buffer, and codegen binds that raw pointer as the `Json`
value (`lower.rs:2793-2806` deliberately does NOT box/dup a union/Json projection, precisely because the
accessor returns an interior pointer, not an ownable box — see the comment there). A fresh `{}` is
`cap 1` with entries stored INLINE in the header (`FLAG_INLINE`); the 2nd key migrates entries to a heap
buffer, the 3rd reallocs — **each grow moves the buffer and dangles every previously-captured
projection pointer.** `for` is only the heap-disturbance trigger: its per-iteration closure-box
allocations reuse the freed block before the dangling pointer is dereferenced (hence "three keys needed").

### Decisive evidence (all reproduced)
- Pre-sizing so the object never grows (`{ "C":[], "B":[], "A":[] }`) + identical for/push pattern → **works** (`done keys=3 C=1 B=1 A=1`).
- Grows but no `.for` → **works** (nothing reuses the freed block before the push).
- The crash needs BOTH the grow (dangles the pointer) AND the intervening allocation (reuses the block).

## Why it matters

`val x = container[k]` followed by any growth of `container` is idiomatic and silently unsafe. It is
latent on master (RC-elision / allocation timing decides whether the freed block is reused) — exactly the
class that flips on unrelated changes (it surfaced when typing `kConnections`; the RAPTOR
`graphResults.mergePathInto` does `bucket = results[head]; bucket.for(...); push(bucket, node)`). It is a
memory-safety hole, not just a crash.

## The design decision (this is the fork — pick one)

The question is the intended aliasing semantics of `val x = container[k]` when `container` is later
mutated/grown. `lin_object_get` has ~10 internal callers plus the codegen Index/spread/destructure paths,
all assuming the interior-pointer contract — so this is a real change, not a one-liner.

1. **Projection materializes an owned box** (`lower.rs` `TypedExpr::Index` union/Json branch, ~2802).
   Simplest at the type level, but it CHANGES OBSERVABLE SEMANTICS: `push(x, …)` would then mutate a
   *copy*, breaking "push through a projected binding mutates the stored array" (which the no-grow case
   relies on today). Needs a decision that projection is by-value.

2. **`lin_object_get` stops returning interior pointers** for values a caller may hold across mutation —
   return the value via a stable indirection / retained box. Preserves mutate-through semantics; cost is
   an extra indirection and an RC-contract ripple across all `lin_object_get` callers + codegen
   Index/spread/destructure.

3. **Object growth must not invalidate outstanding interior pointers** — store entry *values* behind a
   stable indirection (or never realloc the value slots). Most localized to the runtime; preserves
   semantics; costs a pointer-chase per field access and a `LinObject` layout change (mind the inline
   `MakeObject` ABI in codegen).

Tradeoff summary: (1) is cheapest but changes language semantics (projection becomes by-value); (2)/(3)
preserve current mutate-through semantics at a representation cost. The choice is a language-semantics
call: **is `val x = obj[k]` an alias into the container, or a value?** Today it's an (unsound) alias.

## Repro artifact

A minimal, deterministic, pure-stdlib repro is committed at
`benchmarks/compare/raptor/lin/_kconn_repro.lin` (branch `investigate/kconnections-fault`). The
`for`-blame framing in that file's header comment is superseded by THIS doc — the bug is the
interior-pointer/grow UAF above, and `for` is only the allocation trigger.

## Not done

No fix applied — the two prior agents correctly stopped (the localized `for`-release fix is a no-op; a
real fix needs the semantics decision above). Once decided, implement in a fresh worktree with the repro
as a regression test, verified under the debug refcount guard + ASan (leak count must not regress the
~16-leak runtime baseline; no new UAF/double-free), full suite, and the RAPTOR gate.
