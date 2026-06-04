# Lin issues found porting RAPTOR

A prioritized list of Lin language/stdlib problems hit while porting the RAPTOR
journey planner (the only port of the five that needed workarounds). Each has a
minimal repro that was run on this worktree (2026-06-04). Ordered by severity.

The two CORRECTNESS bugs (#1, #2) are the important ones — they silently produce
wrong results or crash, and a real program can hit them without any "scale" excuse.

---

## 1. [CORRECTNESS] `var` written inside an `if` is lost after the branch, in a closure

**Severity: high — silent wrong result.** A `var` declared inside a closure, assigned
inside an `if` branch, and read *after* the branch joins, reads its *initial* value —
the in-branch write is dropped. Looks like a broken SSA phi/merge for closure-local
vars at the branch join.

Repro (prints `[0, 0, 0]`, should be `[2, 2, 2]`):
```lin
import { for } from "std/iter"
import { length, push } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val groups = [[10,11],[20,21],[30,31]]
  var g = 0
  var out: Json = []
  ["a","b","c"].for(id =>
    var sts: Json = []
    if g < length(groups) then
      sts = groups[g]      // <- this write is lost
      g = g + 1            // <- but a write to the OUTER var `g` persists
    push(out, length(sts)) // reads 0, not 2
  )
  print("${toString(out)}")
run()
```

Narrowed (only the combination fails):
- NOT in a closure (plain function body with `var`+`if`): **works**.
- In a closure, reassign with NO `if`: **works**.
- In a closure, reassign in `if` and read INSIDE the same `if`: **works**.
- In a closure, reassign in `if` and read AFTER the `if` joins: **FAILS** (the repro).

Interesting contrast: in the same closure, a write to an *outer*-scope `var` (`g`)
from inside the `if` DOES persist — only the closure-*local* var's in-branch write is
dropped. So the merge logic treats captured-outer and closure-local vars differently.

Workaround used (gtfsLoader.lin trips join): bind via a `val` + conditional expression
instead of a reassigned `var`:
```lin
val matched = g < numGroups && stopTimesGroups[g]["tripId"] == tripId
val sts = if matched then stopTimesGroups[g]["stopTimes"] else []
if matched then g = g + 1
```

---

## 2. [CORRECTNESS] top-level `var` mutated by an exported function in an imported module panics codegen

**Severity: high — compiler crash, not a diagnostic.** The faithful port of the test
helper `t()` wants a module-level `var tripId` that an exported function increments
(`trip${tripId++}`). When that module is *imported* and the function called, codegen
panics:
```
thread 'main' panicked at crates/lin-codegen/src/codegen/mod.rs:782:75:
Binary: undefined lhs temp Temp(0)
```

Repro — module `m.lin`:
```lin
var counter = 0
export val nextId = (): Int32 =>
  counter = counter + 1
  counter
```
main importing it:
```lin
import { nextId } from "./m"
import { print } from "std/io"
import { toString } from "std/string"
print("${toString(nextId())} ${toString(nextId())}")
```
`lin build main.lin` → panic above. (A module-level `var` read/written only WITHIN the
defining module is fine; the bug is the imported-module + exported-mutator combination.)

Workaround (testutil.lin): derive a content-based id from the stop/time signature
instead of a global counter (the tests never assert tripId; `setDefaultTrip` overwrites
it).

---

## 3. [ERGONOMICS / FOOTGUN] Int32 × Int64-literal overflows in Int32 even with an `Int64` target type

**Severity: medium — silent overflow to a wrong (often negative) value.** `x * 1000003i64`
where `x: Int32` computes the product in Int32 and overflows; the `i64` literal operand
does NOT widen `x`, and neither does annotating the result `: Int64`.

Repro (prints `bad=-194043216 ok=90000270000`):
```lin
val x = 90000                     // Int32
val bad: Int64 = x * 1000003i64   // overflow in Int32, THEN widened
val w: Int64 = x                  // explicit widen first
val ok: Int64 = w * 1000003i64    // correct
```

Workaround (bench.lin journeyDigest): widen each operand into its own `Int64` binding
before the arithmetic. Desired: either an explicit `toInt64(v: Int32)` (today
`toInt64` only takes `UInt64`), or have a mixed `Int32 * Int64` operation widen the
Int32 operand (and ideally warn on a narrowing/overflowing multiply feeding an Int64).

---

## 4. [STDLIB / PERFORMANCE] no stable O(n log n) sort; `Json` objects are O(n) per key lookup

**Severity: medium — performance, but it made a real workload look "infeasible".** Two
related gaps surfaced when running the full 240k-trip feed:

a. **No stdlib stable sort.** `std/array.sort` is quicksort (not stable). RAPTOR needs
   a *stable* sort (trip ordering by first departure feeds route grouping + overtaking
   detection). The port shipped a hand-rolled `stableSort`; the first version was an
   insertion sort that **rebuilt the whole array on every insertion** (O(n²) — fine for
   unit tests, ~29 billion boxed copies at 240k trips → 80+ GB RSS, hours). Replacing
   it with a bottom-up merge sort fixed it (now ~feasible). **A stable sort belongs in
   `std/array`** (e.g. `sortStable`) so every program doesn't re-derive one and risk the
   O(n²) trap.

b. **`Json` objects are association lists with O(n) lookup.** `lin_object_get`/
   `lin_object_set` (`crates/lin-runtime/src/object.rs`) linearly scan all entries —
   there is no hashed container. Keying ~16k distinct routeIds while building indexes
   over 240k trips is the dominant cost (Lin's query phase is ~10-50× the hashed-map
   languages). Workaround in the loader: avoid big object maps entirely (contiguous-run
   grouping + sorted-array binary search). **Lin would benefit from a hashed object
   representation or a built-in `Map<K,V>` type** for dictionary-heavy workloads.

---

## 5. [SEMANTICS] dynamic `Json` arithmetic with a missing key: no JS-like NaN, errors at runtime

**Severity: low-medium — porting hazard.** In JS, `number + undefined` is `NaN` and any
comparison with `NaN` is false, so RAPTOR's `scanTransfers` silently skips a transfer
whose destination is not on any route path. In Lin, statically-typed `Int32 + Null` is
correctly *rejected by the type checker* — but when the operands are `Json` (as they are
on the bracket-access path `interchange[stopPi]`), `Json + Json` type-checks and the
missing-key `Null` only bites at runtime.

Repro (type error — the *good* case, shows the checker catches the typed form):
```lin
val obj = { "a": 5 }
val sum = 10 + obj["b"]   // Error: Cannot apply operator Add to Int32 and Null
```
The dynamic-`Json` form slips past the checker and misbehaves at runtime instead.

Workaround (raptor.lin scanTransfers): explicitly guard `ic != null && best != null`
before the arithmetic/comparison, reproducing JS's skip-on-missing. Not a bug per se,
but a documented divergence worth a lint or a defined `Json` numeric-coercion rule.

---

## 6. [ERGONOMICS] minor

- **No CLI argument access wired in the runner.** `run.lin`/`bench.lin` hardcode the
  data dir + query (std exposes args but it wasn't plumbed through here). Low priority.
- **Inline multi-statement closures need newlines**, not `;` — `c => idx[c]=i; i=i+1`
  fails to parse (`Undefined variable ';'`); the newline form works. Expected per the
  grammar, noted only because the error message is misleading.
