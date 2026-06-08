# std/result — ergonomics for `T | Error` and `T | Null`

## Status: proposal

Lin's fallible-value convention is a bare union, not a wrapper ADT. A fallible call returns
`T | Error` (`Error` is the canonical `{ "type": "error", "message": String }`, discriminated
with `is Error`, ADR-031), and an absence-y call returns `T | Null` (`Null` is a first-class
type). There is no `Result`/`Option`, no `Ok`/`Some` constructor, and deliberately so: the value
*is* the success value, with no unwrap ceremony on the happy path, and `match … is Error` reads
plainly. This is a real ergonomic strength — but the moment a caller wants to *transform* or
*chain* a fallible value, the only tool today is a full `match`/`is Error` block, even for a
one-liner. Rust, Scala and Haskell paper over exactly this with `map`/`mapErr`/`unwrapOr`/
`andThen`/`orElse`/`?`. We want those affordances **without** introducing a wrapper type that
would undo the no-ceremony happy path.

This proposal does two things. First, a small `std/result` module of combinators that operate
*directly* on `T | Error` / `T | Null` values — collapsing, mapping, chaining and bridging them.
Second, it evaluates language-level sugar (a `?` propagation operator and a `??` coalescing
operator) as an alternative, and is honest about which of the above can be expressed in today's
argument-driven monomorphized generics and which need compiler work. The headline finding: the
**collapse** and **predicate** helpers are cheap and expressible; the **map/chain** family is the
crux and is *not* cleanly expressible today because Lin generics cannot bind a type variable to
"the non-`Error` arm of a union argument". See [Generics & checker constraints](#generics--checker-constraints).

Note up front: the `??` in the existing docs (object.get, §5.1.1) is **illustrative only** — it
denotes "the defaulted-read idiom", borrowing JS/Kotlin notation. **There is no `??` operator in
Lin today.** Whether to make it real is part of this proposal.

---

## std/result

A module of total, side-effect-free combinators over the two union conventions. Nothing here is a
new runtime type: every function takes a `T | Error` (or `T | Null`) and returns a `T`, a `U`, or
another union of the same shape. The success arm is passed through untouched (same identity, no
clone); the `Error`/`Null` arm is what the combinator decides.

Import:

```txt
import { unwrapOr, isOk, isError, isNull, okOr, toNull, mapOk, mapError, andThen, orElse } from "std/result"
```

Two design rules run through the whole module:

- **`Error` detection is the `is Error` discriminant**, not a bare object-tag check — a
  successfully-decoded object that happens to have a `"type"` field is *not* an error (ADR-031).
- **First-match-wins ordering is respected.** Every function checks the `Error`/`Null` arm
  *first*, mirroring the mandated arm order in hand-written `match` (the `is Error` arm must come
  first, STDLIB §std/json).

### unwrapOr {#unwrapor}

```txt
val unwrapOr: <T>(x: T | Error, default: T) -> T
val unwrapOr: <T>(x: T | Null,  default: T) -> T
```

Collapses the failure arm: returns the success value if `x` is not an `Error`/`Null`, otherwise
returns `default`. The result is a bare `T`, usable with no further guard — this is the
union-error analogue of the `default` arm already on [`array.at`](../../STDLIB.md#at-array) and
[`object.get`](../../STDLIB.md#get), generalized to *any* `T | Error` / `T | Null` value (not just
collection access).

```txt
val port: Int32 = parsePort(input).unwrapOr(8080)        // T | Error  -> T
val name: String = config["name"].unwrapOr("anon")       // T | Null   -> T
```

> Both signatures are written here as an overload pair for clarity. As discussed in
> [Generics & checker constraints](#generics--checker-constraints), the *single* signature
> `<T>(x: T | Error, default: T) -> T` is the one that is hard to infer, because `T` must be
> recovered from "the non-`Error` arm of `x`". The expressible-today fallback is documented there.

### isOk / isError / isNull {#predicates}

```txt
val isOk:    (x: Json) -> Boolean      // true unless x is the canonical Error
val isError: (x: Json) -> Boolean      // true iff x is Error: x["type"] == "error"
val isNull:  (x: Json) -> Boolean      // true iff x is the Null value
```

Boolean predicates for use in `if`/`&&` positions and combinator callbacks where a `match` is
overkill. `isError` is exactly the `is Error` discriminant exposed as a value-returning function;
`isOk` is its negation; `isNull` tests the `Null` arm. They take `Json` (the permissive supertype
that any union flows into) and return a plain `Boolean`.

```txt
results.filter(isOk)                                  // keep successes
if isError(resp) then log(resp["message"]) else …
```

> **Caveat (no narrowing).** Because these return a bare `Boolean`, they do **not** narrow the
> union the way `if x is Error` does (§Type narrowing). After `if isError(x)` the compiler still
> sees `x : T | Error`. For narrowing you must use the built-in `is Error` test. These predicates
> are for filtering/counting, not for unlocking field access on the narrowed arm.

### okOr {#okor}

```txt
val okOr: <T>(x: T | Null, error: Error) -> T | Error
```

Bridges the *absence* convention to the *failure* convention: passes a present value through,
and replaces `Null` with the supplied `Error`. Use it to turn a "missing" into a "failed" so a
`T | Null` can join an `andThen` chain of `T | Error` operations.

```txt
val u: User | Error = lookup(id).okOr(error("no such user: ${id}"))
```

(`error(msg)` is the canonical-`Error` constructor; if it does not already exist it should ship in
`std/result` as `val error: (message: String) -> Error`.)

### toNull {#tonull}

```txt
val toNull: <T>(x: T | Error) -> T | Null
```

The inverse bridge: discards the `Error` *detail*, mapping any failure to `Null`. Use it when the
caller only cares "did it work?" and wants the lighter `T | Null` shape (e.g. to feed
`unwrapOr`/`??` or an optional field).

```txt
val cached: Bytes | Null = readCache(key).toNull()
```

### mapOk {#mapok}

```txt
val mapOk: <T, U>(x: T | Error, f: (T) -> U) -> U | Error
```

Applies `f` to the success value and re-wraps as `U | Error`; passes an `Error` through unchanged
(`f` is not called). The functor `map`. Lets a one-line transform skip the `match`.

```txt
val ageNextYear: Int32 | Error = Person.fromJson(j).mapOk((p) => p.age + 1)
```

> **Feasibility: blocked.** See [Generics & checker constraints](#generics--checker-constraints).
> Binding `T` to the success arm of `x` *and* threading it into `f`'s parameter is not expressible
> in today's argument-driven inference. This is the central reason the proposal recommends
> deferring the map/chain family or backing it with compiler support.

### mapError {#maperror}

```txt
val mapError: <T>(x: T | Error, f: (Error) -> Error) -> T | Error
```

Passes a success value through; rewrites the `Error` arm with `f` (e.g. to add context to the
message). Mirror of `mapOk` on the failure arm.

```txt
val r = readFile(path).mapError((e) => error("loading config: ${e["message"]}"))
```

`mapError` is the *easier* half of the pair: the success arm is `T` (whatever it is) passed
through opaquely, and only the `Error` arm — a known concrete type — is transformed. It does not
need to *name* `T`, only pass it through, which is far closer to expressible (see constraints).

### andThen / flatMap {#andthen}

```txt
val andThen: <T, U>(x: T | Error, f: (T) -> U | Error) -> U | Error
```

Monadic bind: if `x` succeeded, call `f` on the value (which is itself fallible) and return its
result; if `x` is an `Error`, short-circuit and return it. This is the combinator that replaces
*nested* `match` blocks when several fallible steps depend on each other.

```txt
// Three dependent fallible steps, no nesting:
val out: Report | Error =
  readFile(path)
    .andThen((bytes) => Config.fromJson(bytes))
    .andThen((cfg)   => buildReport(cfg))
```

`flatMap` is an exported alias for `andThen` (same function), for readers coming from Scala/Rust.

> **Feasibility: blocked**, same root cause as `mapOk` — `T` and `U` are both arms of unions that
> the checker cannot currently bind from the argument.

### orElse {#orelse}

```txt
val orElse: <T>(x: T | Error, f: (Error) -> T | Error) -> T | Error
```

The dual of `andThen` on the failure arm: pass a success through; on `Error`, call `f` (a fallible
recovery) and return its result. Use for fallback chains ("try cache, else fetch").

```txt
val data = readCache(key).orElse((_) => fetch(url))
```

Like `unwrapOr`, `orElse` must recover `T` from the success arm of `x`, so it shares the
`unwrapOr` feasibility caveat (the recovery side is fine; naming `T` for the pass-through is the
issue).

---

## Generics & checker constraints

This is the crux of the proposal, and the reason it is *honest about feasibility* rather than a
flat "add ten functions". Lin generics have three properties that together make most of the
map/chain family inexpressible **today**:

1. **Inference is argument-driven** (no turbofish, no return-only inference). A type parameter `T`
   is bound by *unifying a concrete argument against a parameter that mentions `T`*. A `T` that
   appears only in the return type, or only inside a callback's *parameter*, never gets a witness
   and cannot be solved.
2. **Monomorphization is per concrete call.** Each call site picks concrete types and the compiler
   emits a specialized copy (§Generic functions, ADR-014). There is no boxed/erased generic path
   for these to fall back on.
3. **A union argument is matched as a whole; the checker has no "non-`Error` arm of `T | Error`"
   destructor.** When you write `(x: T | Error)`, there is no inference rule that, given an
   argument of static type `Person | Error`, *peels* the `Error` and binds `T := Person`. The
   union `T | Error` would unify against `Person | Error` only if the solver knew to subtract the
   literal `Error` arm and bind `T` to the remainder — and that subtraction rule does not exist.
   (Contrast `Number`, which works because the bound is a *family predicate* on a single concrete
   argument, not a union-arm subtraction.)

Walking the helpers against these constraints:

**Collapse helpers (`unwrapOr`) — partially expressible.**
The problem is the `default: T` parameter. If `T` is solved from `default` (a concrete argument),
then `unwrapOr(parsePort(x), 8080)` binds `T := Int32` from `8080`, and `x : Int32 | Error`
unifies against `T | Error` *because `T` is already `Int32`* — unification, not subtraction. So
the **default-driven** form is expressible: `T` flows from the second argument, exactly as
`array.at`'s independent default already does. This is the recommended shape. The form that is
*not* expressible is one that tries to solve `T` from `x` alone (e.g. an `unwrap(x: T | Error) -> T`
with no default — that needs arm-subtraction and would also need a runtime trap on `Error`, which
fights the no-exceptions grain).

**Predicates (`isOk`/`isError`/`isNull`) — fully expressible today.**
They take `Json` and return `Boolean`; no type variable to solve. They ship as-is. Their only
weakness is the no-narrowing caveat noted above — a *value-returning* predicate cannot drive
flow-narrowing, which is a deliberate language rule (only `is`/`has`/`if`/`&&` narrow).

**Bridges (`okOr`, `toNull`) — borderline.**
`toNull(x: T | Error) -> T | Null` needs to bind `T` from `x`'s success arm (arm-subtraction
again) *and* reconstruct `T | Null` in the return — both blocked. `okOr` is the same. They are
only expressible if the checker gains arm-subtraction (see below), **or** if we accept a degraded
`Json`-typed signature (`toNull: (x: Json) -> Json`) that loses the static success type — which
defeats the purpose. Verdict: defer until arm-subtraction exists.

**Map/chain (`mapOk`, `andThen`, `orElse`) — blocked.**
These need *both* arm-subtraction (`T` from `x`) *and* threading that `T` into a callback
parameter. `mapOk(x: T | Error, f: (T) -> U)` cannot solve `T` (it appears only as a union arm and
as a callback parameter, never as a bare concrete argument) and cannot solve `U` (return-only,
through the callback's return). Both fail rule 1 and rule 3. **`mapError` is the lone exception
that is close to expressible:** `T` is only passed through opaquely (it never needs naming if the
checker can keep the success arm as an unsolved-but-carried type), and the `Error` arm is a known
concrete type, so `f: (Error) -> Error` has no free variable. Even `mapError` may stumble on
carrying the un-named success arm through the return union, but it is the one to prototype first.

### What compiler support would unlock the family

A single, well-scoped checker feature unblocks the whole map/chain/bridge family: **union-arm
subtraction in inference** — a rule that, when unifying a generic parameter `T | Error` (or
`T | Null`) against a concrete union argument, binds `T` to the argument's union *minus* the
literal `Error` (resp. `Null`) arm. With that one rule:

- `mapOk`/`andThen`/`orElse`/`okOr`/`toNull` all become expressible (`T` is solved from `x`; `U`
  in `mapOk`/`andThen` is still return-/callback-driven and needs the *second* piece below).
- `unwrapOr` without a default becomes expressible (but still should not trap, so keep the
  default).

A *second*, larger piece is **callback-parameter inference** (solving `U` in `mapOk` from `f`'s
return type, and propagating the solved `T` *into* `f`'s parameter so the callback body type-checks
without annotation). This is the harder one and overlaps with the already-noted `fromEntries`
limitation (a type parameter nested in a callback/array argument is not yet inferable, STDLIB
§std/object). Until it lands, even with arm-subtraction the user would have to annotate the
callback parameter (`(p: Person) => …`), which is tolerable but not the clean Rust experience.

**Honest verdict on the library route:** of the ten requested helpers, **three ship today**
(`isOk`, `isError`, `isNull`), **one ships today in its default-driven shape** (`unwrapOr`), and
**six are blocked** on checker work (`mapOk`, `mapError`, `andThen`, `orElse`, `okOr`, `toNull`),
with `mapError` the best candidate for an early prototype. A pure-library `std/result` cannot
deliver the headline map/chain ergonomics without a compiler change.

---

## Language sugar alternative (`?` / `??`)

Because the most valuable combinators are exactly the ones blocked by the type system, language
sugar deserves serious weight here — sugar is checked by *purpose-built typing rules*, so it
sidesteps the generic-inference wall entirely.

### `??` — nullish/error coalescing (proposed; not real today)

`??` does **not** exist as an operator today; the docs use it only as notation for "defaulted
read". Making it real is the highest-value, lowest-risk addition in this whole proposal:

```txt
x ?? default
```

- **Typing.** If `x : T | Null`, then `x ?? d` has type `T | typeof(d)` — collapsing the `Null`
  arm to `d`. If `x : T | Error`, the `Error` arm is collapsed instead. The result type is exactly
  what the `unwrapOr` overloads produce, but computed by a dedicated binary-operator rule, so **no
  generic inference is involved** — the arm-subtraction is done structurally by the operator's
  typing rule, which the checker can do directly even though the *function* form cannot.
- **Evaluation.** Short-circuits: `default` is evaluated only when `x` is `Null`/`Error`.
- **Why it wins.** It is the operator form of `unwrapOr`/`get`'s default, gives a uniform spelling
  for the idiom the docs *already advertise*, and unifies the `T | Null` and `T | Error` collapse
  into one token. It composes with `array.at`/`object.get` (whose `default` arg becomes redundant
  for the bare case: `arr.at(i) ?? 0`).

This is recommended **for adoption**. It is small, it matches Kotlin/JS/C# intuition, and it pays
for the single most common fallible-value operation (collapse-with-default) without any of the
generics pain.

### `?` — Error propagation (proposed; weigh carefully)

A postfix `expr?` that **early-returns** the `Error` arm and evaluates to the success value:

```txt
val loadReport = (path: String): Report | Error => {
  val bytes = readFile(path)?          // if Error, return it from loadReport
  val cfg   = Config.fromJson(bytes)?  // bytes is now Bytes, cfg is Config | Error -> Config
  buildReport(cfg)
}
```

- **Typing.** `expr?` requires `expr : T | Error` and the *enclosing function's* return type to be
  some `… | Error`; it narrows the binding to `T` on the success path and inserts a
  `return <the Error>` on the failure path. (A `T | Null` variant returning `Null` is conceivable
  but muddier; recommend `?` for `Error` only, and `??` for `Null`.)
- **What it buys.** It is precisely `andThen`-chaining without the closures — the blocked combinator
  delivered as control flow. It collapses the deeply-nested `match` ladders that motivate this
  whole proposal.
- **What it costs (the grain conflict).** Lin's stated philosophy favors **explicit `match`** and
  has *no exceptions / no hidden control flow*. A `?` introduces an invisible early return mid-
  expression — exactly the kind of non-local control flow the language otherwise avoids. It also
  needs a rule for "enclosing function must return `… | Error`" and interacts with `var`-narrowing
  invalidation. It is the highest-power, highest-philosophical-cost feature here.

Recommendation: **defer `?`**, prototype it behind discussion, and revisit once `??` and the
expressible helpers are in real use. If the residual pain is *chaining* (not *defaulting*), `?` is
the right answer; if it is mostly defaulting, `??` alone suffices and `?` is not worth the grain
conflict.

### Sugar vs library, summarized

| Need | Library | Sugar | Recommendation |
|---|---|---|---|
| Collapse with default | `unwrapOr` (default-driven, ships today) | `??` (proposed) | Ship `unwrapOr` now; adopt `??` as the idiomatic spelling |
| Predicate / filter | `isOk`/`isError`/`isNull` (ship today) | — | Ship now |
| Transform success (`map`) | `mapOk` (blocked) | covered by `?` + plain expr | Defer lib; `?` makes it unnecessary |
| Chain fallible (`andThen`) | `andThen` (blocked) | `?` (proposed) | Prefer `?` over the closure form |
| Recover (`orElse`) | `orElse` (blocked) | `x ?? fallback()` (partial) | `??` covers the common case |
| Bridge Null↔Error | `okOr`/`toNull` (blocked) | — | Defer until arm-subtraction |

---

## Recommendation

**Minimal viable set — ship now (pure library, no compiler change):**

1. `isOk`, `isError`, `isNull` — trivially expressible, immediately useful for filtering/branching.
2. `unwrapOr` in its **default-driven** shape (`<T>(x: T | Error, default: T) -> T` and the
   `T | Null` overload), where `T` is solved from `default`. This is the same mechanism that
   already powers `array.at`/`object.get` defaults, so it is known-good.
3. `error(message: String) -> Error` constructor (if not already present), needed by the rest.

**Adopt the one high-value language feature:**

4. `??` coalescing operator. It delivers the most common fallible operation (collapse-with-default,
   for *both* `T | Null` and `T | Error`) with a dedicated typing rule that sidesteps the generic-
   inference wall, matches the notation the docs already use, and carries minimal philosophical
   cost. This is the single best ergonomics-per-effort item in the proposal.

**Defer to a language/checker change (do not ship as a degraded `Json`-typed library):**

5. `mapOk`, `andThen`/`flatMap`, `orElse`, `okOr`, `toNull` — blocked on **union-arm subtraction
   in inference** (and, for `mapOk`/`andThen`, **callback-parameter inference**). Implement the
   arm-subtraction rule first; it unblocks the bridges and the map/chain family in one stroke.
   `mapError` is the recommended early prototype (closest to expressible).

**Defer pending real-world demand:**

6. The `?` propagation operator. It is the most powerful answer to the *chaining* pain but the most
   in tension with Lin's explicit-`match`, no-hidden-control-flow grain. Revisit after `??` and the
   shippable helpers are in use; adopt only if chaining (not defaulting) proves to be the dominant
   residual friction.

**Net verdict.** A useful slice ships today with zero compiler work (`isOk`/`isError`/`isNull` +
default-driven `unwrapOr`), and the single most valuable ergonomic — `??` — is achievable as a
small, grain-respecting operator. But the marquee Rust-style `map`/`andThen` combinators are **not
expressible in Lin's argument-driven monomorphized generics today**: they require a new union-arm-
subtraction inference rule, without which a library `std/result` can only offer them in a type-
erased `Json` form that throws away the static guarantees that make them worth having. The right
sequencing is: ship the cheap helpers + `??`, add arm-subtraction to the checker, then revisit the
full module and the `?` operator.
