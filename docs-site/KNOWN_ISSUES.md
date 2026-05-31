# Known compiler issues hit while writing the docs builder

The docs builder is written in Lin and exercises the compiler fairly hard. Two
codegen reference-counting bugs surfaced while writing it. One was fixed; the
other is worked around in the builder and documented here so it can be fixed
properly (it needs ASan verification, which `cargo test` alone won't catch).

## 1. FIXED — `split()`/`lines()` elements were tagged `TAG_NULL`

`lin_string_split` pushed each element into the result array with element tag `0`
(`TAG_NULL`) instead of `TAG_STR`. Index access (`parts[i]`) worked because codegen
knows the static `String[]` element type and reads the payload directly, but the
generic `for`/`map` path reads the runtime element tag — so iterating a `split()`
result yielded `null` for every element (and leaked the strings on array release).
`lines()` routes through the same function, so it was affected too.

Fixed in `crates/lin-runtime/src/string.rs` (tag elements `TAG_STR`); regression
tests in `crates/lin/tests/integration.rs` (`test_split_result_iterates_as_strings`)
and `stdlib/string.test.lin`.

## 2. OPEN — returning a heap field projected out of a locally-bound value frees it

Minimal reproduction:

```lin
val mk = (): Json => { "blocks": [{ "k": 1 }, { "k": 2 }] }

val bad = (): Json =>
  val s = mk()
  s["blocks"]        // returns an array that reads as length 0 at the call site

val arr = bad()
print(toString(length(arr)))   // prints 0, should print 2
```

The emitted IR releases the container `s` *before* cloning the projected interior
value for the function result:

```llvm
call void @lin_tagged_release(ptr %call)              ; frees s and its "blocks" array
%clone = call ptr @lin_tagged_clone(ptr %projection)  ; clones already-freed memory
ret ptr %clone
```

So the escape-clone (lin-ir `lower.rs`, the `is_union_ty` return path ~line 3861)
fires but in the wrong order relative to the container release — a use-after-free.
It reproduces deterministically for a `Json`/union-typed projection returned as a
function result, and also when the projection's siblings are released early by a
recursive call inside a string interpolation (see `processLinks` in
`builder/markdown.lin`).

**Workarounds used in the builder** (both verified):
- Return an independent copy: `concat([], finalState["blocks"])` instead of the bare
  projection (`builder/markdown.lin`, `parseBlocks`).
- Bind a recursive call to a `val` before interpolating, so sibling locals stay live
  (`builder/markdown.lin`, `processLinks`).

**Suggested fix:** in the union/Json escape-clone path, emit the `CloneBox` of the
returned projection *before* the scope-exit release of the container it points into,
or retain the projected value at projection time when it escapes. Verify under ASan
(`.github/workflows/ci.yml` has an instrumented leg) — `cargo test` will not catch
the UAF on its own.

## 3. OPEN (pre-existing) — `for` over a heap-element array leaks the elements

Iterating an array whose elements are heap values (strings/objects/arrays) with `.for`
leaks one element per iteration. Reproduces without `split` (so it is unrelated to
issue 1):

```lin
import { for, range, push } from "std/array"
range(0, 1000).for(n =>
  var parts = []
  push(parts, "${n}x")
  push(parts, "${n}y")
  var acc = ""
  parts.for(s => acc = "${acc}${s}")   // leaks the two heap strings each iteration
)
```

Under `ASAN_OPTIONS=detect_leaks=1` this leaks ~2 objects/iteration, allocated by the
string constructor; a literal `["a","b"]` array shows no leak because its elements are
interned (immortal) strings. The leak is in the `for`-over-tagged-array path (the
per-iteration element box, or the source array not being released at scope exit), not
in `map`/`split` specifically — `map` leaks only because it iterates with `for`
internally.

This is **pre-existing** (it predates the docs work) and does not crash — it is a
steady leak, harmless for a short-lived build process like the docs generator. CI runs
the ASan leg with `detect_leaks=0` because the stdlib also retains program-lifetime
globals, so this is not currently caught. Fixing it is a focused RC change to the
`for` lowering / element-box reclaim and should be done with the ASan leg flipped to
`detect_leaks=1` for the affected cases.
