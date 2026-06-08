## Status: proposal

# std/random

A full-featured pseudo-random number module: a *seedable, reproducible* generator plus
distribution helpers and the generic array operations (`shuffle`, `choice`, `sample`) that
every game, simulation, and test fixture needs. Today the only randomness in the standard
library is [`std/math.random`](../../STDLIB.md#stdmath) — a single `random() -> Float64`
that returns a uniform value in `[0, 1)`, drawn from non-seedable global state. That is fine
for a quick jitter value, but it cannot be reproduced: there is no way to fix the seed, so a
failing test or a divergent simulation can never be replayed. `std/random` fills that gap
with an explicit, seedable `Rng` handle — the same affordance Python's `random.Random`, Go's
`math/rand.New(rand.NewSource(seed))`, Rust's `StdRng::seed_from_u64`, and Scala's
`scala.util.Random(seed)` all provide. The Lin SDL example projects (procedural maps, particle
jitter) and the `std/test` suites are the immediate consumers: a fixed seed makes a stochastic
scene or a property test deterministic and debuggable.

This module is explicitly **NOT** for security. Its generator is a fast, statistically-good but
fully predictable PRNG: given the seed (or a couple of outputs) an attacker can reconstruct the
entire stream. For tokens, nonces, salts, key material, or anything an adversary must not guess,
use `std/crypto.randomBytes` (a separate proposal) — a CSPRNG seeded from the operating system.
The contrast in one line: `std/random` is for *reproducibility and speed*; `std/crypto` is for
*unpredictability*.

---

## Design overview

### Two layers: global convenience + explicit handle

Following Python and Rust, the API comes in two layers over the *same* algorithm:

1. **Module-level convenience functions** (`randInt`, `randFloat`, `bool`, `shuffle`, `choice`,
   `sample`) operate on a lazily-initialised, entropy-seeded *thread-local* global generator.
   These are the everyday ergonomic path — no handle to thread through.
2. **An explicit opaque `Rng` handle** with method-style equivalents
   (`rng.randInt(...)`, `rng.shuffle(...)`, …). You construct it with `seed(n)` for
   reproducibility or `fromEntropy()` for an independent unpredictable-but-still-not-secure
   stream. This is the path for tests, simulations, and any code that must replay a stream or
   isolate generators across worker threads.

Because Lin's dot-application means `rng.randInt(0, 10) == randInt(rng, 0, 10)`, the handle
methods are literally the convenience functions with an `Rng` first parameter. Every helper
therefore has two signatures: the global one (no `Rng`) and the handle one (leading `Rng`).

### The `Rng` opaque handle

```txt
type Rng
```

`Rng` is an **opaque handle**, declared in the `std/random` source the same way `std/time`'s
`Timer` is: the runtime represents it as a single boxed scalar (a `Json`-erased pointer to a
heap cell holding the 128-bit generator state), and user code may only pass it to `std/random`
functions — it has no observable fields. Mutating a generator (drawing a number advances its
internal state) happens *through* the handle: an `Rng` is a mutable reference, like an array, so
`rng.randInt(...)` advances the same generator the handle points at. This is the one place in
this module with observable mutation, and it is deliberate — a generator with no state would not
be a generator. (Contrast the array helpers below, which are non-mutating.)

### Range conventions

`randInt(lo, hi)` is **half-open `[lo, hi)`** — `hi` is exclusive — to match
[`range()`](../../STDLIB.md) and standard slicing throughout Lin (and Python/Rust/Go `Intn`).
This makes `randInt(0, arr.length())` a valid index into `arr` with no off-by-one. A separate
`randIntInclusive(lo, hi)` covers the `[lo, hi]` case (e.g. a die roll `randIntInclusive(1, 6)`)
without ambiguity. `randFloat(lo, hi)` returns `[lo, hi)`.

### Mutating vs non-mutating array ops

`shuffle`, `choice`, and `sample` are **non-mutating**: they read the input array and return a
new array (or element), leaving the argument untouched — the same convention as
[`std/array.reverse`](../../STDLIB.md) and `sort`. This contrasts with the in-place primitives
`push`/`set`. A `shuffle` that copied-then-permuted matches the predictable, expression-oriented
style of the rest of `std/array`. For callers that explicitly want an in-place permutation of a
large buffer (avoiding the copy), `shuffleInPlace<T>(arr: T[]) -> Null` is provided as the
documented mutating counterpart, named like `push`/`set` so the mutation is visible at the call
site.

---

## Specification

Import:

```txt
import { seed, randInt, randFloat, shuffle, choice, sample } from "std/random"
```

### Summary

| Function | Signature | Summary |
| --- | --- | --- |
| [`seed`](#seed) | `(n: Int64) -> Rng` | Create a reproducible generator from a 64-bit seed |
| [`fromEntropy`](#fromentropy) | `() -> Rng` | Create an entropy-seeded (non-reproducible, non-secure) generator |
| [`randInt`](#randint) | `([Rng,] lo: Int32, hi: Int32) -> Int32` | Uniform integer in `[lo, hi)` |
| [`randIntInclusive`](#randintinclusive) | `([Rng,] lo: Int32, hi: Int32) -> Int32` | Uniform integer in `[lo, hi]` |
| [`randFloat`](#randfloat) | `([Rng,] lo: Float64, hi: Float64) -> Float64` | Uniform float in `[lo, hi)` |
| [`next`](#next) | `([Rng]) -> Float64` | Uniform float in `[0, 1)` (the seedable analogue of `math.random`) |
| [`bool`](#bool) | `([Rng,] p: Float64) -> Boolean` | Bernoulli trial: `true` with probability `p` |
| [`shuffle`](#shuffle) | `<T>([Rng,] arr: T[]) -> T[]` | A new array with `arr`'s elements in random order |
| [`shuffleInPlace`](#shuffleinplace) | `<T>([Rng,] arr: T[]) -> Null` | Permute `arr` in place (mutating) |
| [`choice`](#choice) | `<T>([Rng,] arr: T[]) -> T \| Null` | One random element; `Null` if empty |
| [`sample`](#sample) | `<T>([Rng,] arr: T[], k: Int32) -> T[]` | `k` distinct elements, without replacement |

Each function listed with `[Rng,]` has two real signatures: a **global** form (omit the `Rng`,
uses the thread-local default generator) and a **handle** form (leading `Rng`, dot-callable as
`rng.f(...)`). They are documented together below.

---

### seed

```txt
val seed: (n: Int64) -> Rng
```

Returns a new generator deterministically initialised from the 64-bit seed `n`. Two generators
created with the same seed produce identical sequences across runs, platforms, and Lin versions
(the algorithm is pinned — see *Implementation notes*). This is the reproducibility primitive:
seed from a test constant to make a stochastic test deterministic, or from a saved value to
replay a simulation.

```txt
val rng = seed(42)
rng.randInt(0, 100)   // e.g. 17 — and ALWAYS 17 for seed 42 on the first draw
```

The seed is scrambled (run through a SplitMix64 mixing step) before use, so low-entropy or
sequential seeds (`seed(0)`, `seed(1)`, `seed(2)`) still yield well-separated streams.

---

### fromEntropy

```txt
val fromEntropy: () -> Rng
```

Returns a new generator seeded from a non-deterministic source (the OS entropy pool, falling
back to a time/PID mix). Each call yields an independent, effectively-unpredictable-to-a-casual-
observer stream — but **not** a cryptographic one. Use this for an independent generator on a
worker thread, or when you want variety without caring about replay. For anything an adversary
must not predict, use `std/crypto.randomBytes`.

---

### randInt

```txt
val randInt: (lo: Int32, hi: Int32) -> Int32           // global
val randInt: (rng: Rng, lo: Int32, hi: Int32) -> Int32 // handle
```

Returns a uniformly-distributed integer in the **half-open** interval `[lo, hi)` — `lo`
inclusive, `hi` exclusive. The distribution is unbiased (rejection sampling, not modulo). It is
a runtime error if `lo >= hi` (the interval is empty).

```txt
randInt(0, 6)          // 0, 1, 2, 3, 4, or 5 — never 6
randInt(0, arr.length()) // a valid index into arr

val rng = seed(7)
rng.randInt(10, 20)    // reproducible draw in [10, 20)
```

---

### randIntInclusive

```txt
val randIntInclusive: (lo: Int32, hi: Int32) -> Int32
val randIntInclusive: (rng: Rng, lo: Int32, hi: Int32) -> Int32
```

Like `randInt` but the interval is **closed** `[lo, hi]` — both ends included. It is a runtime
error if `lo > hi`. Use for natural inclusive ranges such as dice or calendar days.

```txt
randIntInclusive(1, 6)   // a six-sided die: 1..6 inclusive
```

---

### randFloat

```txt
val randFloat: (lo: Float64, hi: Float64) -> Float64
val randFloat: (rng: Rng, lo: Float64, hi: Float64) -> Float64
```

Returns a uniformly-distributed `Float64` in `[lo, hi)`. With `lo == hi` it returns `lo`.

```txt
randFloat(0.0, 1.0)     // like math.random(), but seedable via a handle
randFloat(-1.0, 1.0)    // signed jitter
```

---

### next

```txt
val next: () -> Float64
val next: (rng: Rng) -> Float64
```

Returns a uniformly-distributed `Float64` in `[0, 1)` — the direct, seedable analogue of
[`math.random`](../../STDLIB.md#stdmath). `next()` (global) and `math.random()` produce the same
*kind* of value; `next` exists so a seeded `Rng` can supply the same `[0, 1)` primitive that
higher-level helpers are built on. Prefer `randFloat`/`randInt` when you have explicit bounds.

---

### bool

```txt
val bool: (p: Float64) -> Boolean
val bool: (rng: Rng, p: Float64) -> Boolean
```

A Bernoulli trial: returns `true` with probability `p` and `false` with probability `1 - p`.
`p` is clamped to `[0, 1]` (so `bool(2.0)` is always `true`, `bool(-1.0)` always `false`).
`bool()` with the default `p` of `0.5` is a fair coin flip.

```txt
bool(0.5)    // fair coin
bool(0.1)    // true ~10% of the time
```

> `p` defaults to `0.5`: `val bool = (p: Float64 = 0.5): Boolean => …`.

---

### shuffle

```txt
val shuffle: <T>(arr: T[]) -> T[]
val shuffle: <T>(rng: Rng, arr: T[]) -> T[]
```

Returns a **new** array containing all of `arr`'s elements in a uniformly-random order, using an
unbiased Fisher–Yates (Knuth) shuffle. The input array is **not modified** (matching `std/array`'s
`reverse`/`sort`). Generic over `T`, so `shuffle(Int32[])` returns an `Int32[]` with the packed
scalar representation preserved.

```txt
val deck = range(0, 52)
val shuffled = deck.shuffle()    // deck is unchanged
```

---

### shuffleInPlace

```txt
val shuffleInPlace: <T>(arr: T[]) -> Null
val shuffleInPlace: <T>(rng: Rng, arr: T[]) -> Null
```

Permutes `arr` **in place** (no copy) and returns `Null`. The mutating counterpart to `shuffle`,
named like the in-place primitives `push`/`set` so the side effect is explicit at the call site.
Use when shuffling a large buffer and the original order is not needed.

```txt
val cards = range(0, 52)
cards.shuffleInPlace()   // cards is now permuted
```

---

### choice

```txt
val choice: <T>(arr: T[]) -> T | Null
val choice: <T>(rng: Rng, arr: T[]) -> T | Null
```

Returns one uniformly-random element of `arr`. On an **empty array** it returns `Null` rather
than raising a runtime error — this is the safer, idiom-aligned choice (mirrors
[`array.at`](../../STDLIB.md), which returns `T | Null` rather than trapping), and forces the
caller to handle the empty case at compile time via the union. (Python's `random.choice` raises
on empty; Lin prefers the total `T | Null` signature.)

```txt
val pick = ["red", "green", "blue"].choice()   // String | Null
match pick {
  Null => "no colours"
  c => c
}
```

---

### sample

```txt
val sample: <T>(arr: T[], k: Int32) -> T[]
val sample: <T>(rng: Rng, arr: T[], k: Int32) -> T[]
```

Returns a new array of `k` **distinct** elements drawn from `arr` **without replacement** (no
element appears twice), in random order. It is a runtime error if `k < 0` or `k > arr.length()`
(you cannot draw more distinct items than exist — like Python's `random.sample`). `sample(arr, 0)`
returns `[]`; `sample(arr, arr.length())` is a full shuffle. The input is not modified.

```txt
val winners = entrants.sample(3)   // 3 distinct entrants, random order
```

---

## Implementation notes

**PRNG algorithm.** Use **PCG-XSH-RR 64/32** (O'Neill's PCG, 64-bit state, 32-bit output) seeded
through a **SplitMix64** mixing step. Rationale:

- It is fast (a multiply, an add, two shifts, a rotate per draw), small (128 bits of state: a
  64-bit LCG state + a 64-bit increment), and passes the standard statistical batteries
  (TestU01 BigCrush, PractRand) far better than the legacy LCG or `rand()` that `math.random`
  may sit on. It is the default in Rust's `rand` (`StdRng` is a CSPRNG, but PCG is the canonical
  `SmallRng`) and is widely used in simulation work.
- SplitMix64 on the incoming seed guarantees `seed(0)`, `seed(1)`, `seed(2)` give well-separated
  streams, so users do not need to pre-hash their seeds.
- xoshiro256** is a reasonable alternative (slightly larger state, no jump-ahead in this design);
  PCG is preferred for its compact state and proven seeding story. The choice is documented and
  **pinned**: the algorithm and seed-scrambling must not change across Lin versions, or `seed(n)`
  reproducibility silently breaks. Treat the sequence as part of the module's contract (add a
  golden-vector test: `seed(42)` ⇒ a fixed list of outputs).

**Pure-Lin vs runtime intrinsic.** PCG is pure 64-bit integer arithmetic (wrapping multiply/add,
shifts, rotate). It *could* be written in pure Lin over `Int64`, which would make the sequence
trivially identical across all targets — attractive for reproducibility. However:

- Lin `Int64` arithmetic must wrap (two's-complement, no overflow trap) for the LCG step; confirm
  the language guarantees wrapping `*`/`+` on `Int64` (it does for the codegen target). If wrapping
  is guaranteed, a **pure-Lin core** is the recommended implementation — it has no FFI surface,
  is portable by construction, and the `Rng` state can be a small sealed record `{ state: Int64,
  inc: Int64 }` rather than an opaque foreign handle.
- The recommended hybrid: implement the **PRNG core in pure Lin** (state record + advance/output
  functions), and use **one runtime intrinsic only for `fromEntropy`** (`lin_random_entropy() =>
  Int64`, reading the OS entropy source / time-PID fallback). `seed`, `randInt`, `randFloat`,
  `bool`, `shuffle`, `choice`, `sample` are then all pure Lin built on the core and `std/array`.

**The `Rng` handle.** Two viable representations:
  1. A **sealed record** `type Rng = { state: Int64, inc: Int64 }` (preferred if the pure-Lin core
     is used) — concrete, fast, and the mutable-advance is just field reassignment through the
     reference. Export it as an *opaque* type (export the name, not the fields) so users cannot
     poke at the state and the layout can change without breaking callers.
  2. An **opaque foreign handle**, erased to `Json`/`Int64` exactly like `std/time`'s `Timer`, if
     the core lives in the runtime. Heavier (FFI boundary, heap cell) and not needed if the pure-Lin
     path works — recommend representation (1).

  Either way the handle is a **mutable reference**: drawing advances the state in place, so the
  same `Rng` passed to successive calls yields successive (not repeated) draws. Across worker
  threads, generators must not be shared (the deep-copy transfer would clone the state, silently
  diverging); document that each thread should `seed`/`fromEntropy` its own `Rng`. The global
  convenience generator is **thread-local** for the same reason.

**Stand alone vs extend std/math.** Recommend a **standalone `std/random` module**, not extending
`std/math`. `std/math` is a thin, pure, *stateless* wrapper over libm; a seedable generator
introduces mutable handles, generic array operations, and a dependency on `std/array` (for
`shuffle`/`sample`), none of which belong in a numeric-functions module. Keep `math.random` exactly
as-is (the zero-ceremony `[0,1)` global) and point its docs at `std/random` for seedable/array use,
and at `std/crypto.randomBytes` for security. `std/random.next()` is the explicit seedable bridge
between the two.

**Bias-free integer generation.** `randInt`/`randIntInclusive`/`choice`/`sample` must use
rejection sampling (Lemire's bounded-multiply or a reject-above-threshold loop), **not** `output %
range`, to avoid modulo bias for ranges that do not divide the generator's period evenly. Document
this so it is not "optimised" into a biased modulo later.

**RC / representation.** `shuffle`/`sample` build new arrays; reuse `std/array`'s generic
push/slice machinery so packed scalar arrays (`Int32[]`, `Float64[]`) stay packed and heap-element
arrays take exactly one net retain per moved element (verify under AddressSanitizer, per the
ADR-059 array RC discipline). `shuffleInPlace` swaps elements within the existing buffer — for a
packed scalar array this is a pure value swap (no RC traffic); for a boxed/Json array the swap
moves pointers without changing refcounts.

---

## Summary

`std/random` adds a seedable, reproducible PRNG (PCG seeded via SplitMix64) to Lin, exposed both
as a thread-local global for convenience and as an explicit opaque `Rng` handle for the
reproducibility that tests, SDL examples, and simulations require — filling the gap left by the
non-seedable `math.random`. It provides bias-free integers (`randInt` half-open `[lo, hi)` to match
`range()`, plus an inclusive variant), `randFloat`, a Bernoulli `bool`, and generic, non-mutating
`shuffle`/`choice`/`sample` (with an in-place `shuffleInPlace` counterpart and an empty-safe
`choice -> T | Null`). It is explicitly a fast, predictable generator for reproducibility and
speed — **not** cryptographic; security-sensitive randomness must use `std/crypto.randomBytes`,
and the recommended implementation is a pure-Lin PCG core (`Rng` as an opaque sealed record) with
a single `fromEntropy` runtime intrinsic, standing alone rather than extending `std/math`.
