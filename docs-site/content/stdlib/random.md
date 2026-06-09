# std/random

std/random — deterministic, seedable pseudo-random numbers.

Not cryptographically secure. This module is a fast, reproducible, statistically good PRNG for
simulations, sampling, shuffling, games and tests — not for passwords, tokens, keys, nonces or
anything an adversary must not predict. Its entire state is recoverable from a handful of
outputs. For secure randomness (key material, session tokens, salts) use `std/crypto` instead.

── Algorithm ──────────────────────────────────────────────────────────────────────────────
The generator is PCG (XSH-RR 64/32: 64-bit LCG state, 32-bit permuted output), seeded by running
a SplitMix64 mixer over the user seed so that even a low-entropy seed like `0` or `1` produces
well-distributed state. OS entropy is used solely to seed `fromEntropy()`.

── Two APIs ───────────────────────────────────────────────────────────────────────────────
  • Explicit handle (pure, reproducible): an opaque `Rng` value carries the generator state.
    Every draw returns a `{ value, rng }` pair — the drawn value plus the next `Rng` to thread
    into the following draw. Nothing mutates; the same `Rng` always yields the same draw, which
    is what makes a run reproducible. Build one with `seed(n)` (deterministic) or `fromEntropy()`
    (OS-seeded, non-reproducible).

```lin
• Global convenience: `next`/`int`/`float`/`boolean`/`pick`/`shuffled` draw from a single
  module-level generator (seeded from OS entropy at load). Convenient for scripts; call
  `reseed(n)` to make the global stream reproducible. The global generator is per-process state
  of the thread of execution (workers receive their own deep-copied seed) — for concurrent
  reproducibility prefer the explicit-handle API.
```

`randInt`/`int` are half-open `[lo, hi)`, matching `range(lo, hi)` from `std/iter`.

## Reference

### Opaque handle

#### `Rng`

```lin
type Rng = { "state": UInt64, "inc": UInt64 }
```

Treat `Rng` as opaque: its fields are an implementation detail and may change. It is copied by
value, never mutated in place — each draw builds a fresh `Rng`.

#### `IntDraw`

```lin
type IntDraw = { "value": Int32, "rng": Rng }
```

The result of an `Int32` draw: the drawn `value` plus the advanced generator to thread onward.

#### `FloatDraw`

```lin
type FloatDraw = { "value": Float64, "rng": Rng }
```

The result of a `Float64` draw (in `[0, 1)`) plus the advanced generator to thread onward.

#### `BoolDraw`

```lin
type BoolDraw = { "value": Boolean, "rng": Rng }
```

The result of a `Boolean` draw plus the advanced generator to thread onward.

#### `Draw`

```lin
type Draw<T> = { "value": T, "rng": Rng }
```

The result of a generic draw of some `T` plus the advanced generator to thread onward (used by
`choice`/`shuffle`/`sample`).

### Constructors

#### `seed`

```lin
val seed = (n: UInt64): Rng
```

Build a generator from a numeric seed. Deterministic: the same `n` always yields the same
sequence (even low-entropy seeds like `0`/`1` are well-distributed).
- **`n`** — the seed; an `Int32` is accepted ergonomically and widened to the `UInt64` seed space.
- **Returns** a fresh `Rng` to thread into the first draw.

#### `fromEntropy`

```lin
val fromEntropy = (): Rng
```

Build a generator seeded from operating-system entropy. Non-reproducible by design (each call
produces an independent stream). Use this for "different numbers each run"; use `seed(n)` to
replay a run.
- **Returns** a fresh OS-seeded `Rng`.

### Explicithandle draws

#### `nextInt`

```lin
val nextInt = (r: Rng): IntDraw
```

Draw the next raw 32-bit value. Prefer `randInt`/`float` for bounded draws.
- **`r`** — the generator state.
- **Returns** an `IntDraw`; `value` is the full 32-bit pattern reinterpreted (may be negative), and
         `rng` is the advanced generator to thread onward.

#### `float`

```lin
val float = (r: Rng): FloatDraw
```

Draw a uniform `Float64` in `[0, 1)`. Has 32 bits of resolution (sufficient for sampling/jitter;
not a full-mantissa double).
- **`r`** — the generator state.
- **Returns** a `FloatDraw` with `value` in `[0, 1)` and the advanced `rng`.

#### `randInt`

```lin
val randInt = (r: Rng, lo: Int32, hi: Int32): IntDraw
```

Draw a uniform integer in the half-open range `[lo, hi)` (like `range(lo, hi)`). Uses modulo
reduction — negligible bias for the small ranges typical of dice/indices/sampling, but not a
uniform-crypto primitive.
- **`r`** — the generator state.
- **`lo`** — the inclusive lower bound.
- **`hi`** — the exclusive upper bound.
- **Returns** an `IntDraw`; when `hi <= lo` the range is empty and `value` is `lo` (the generator is
         still advanced for predictability).

#### `boolean`

```lin
val boolean = (r: Rng): BoolDraw
```

Draw a uniform `Boolean` (a coin flip).
- **`r`** — the generator state.
- **Returns** a `BoolDraw` with the flip and the advanced `rng`.

#### `choice`

```lin
val choice = <T>(r: Rng, xs: T[]): Draw<T | Null>
```

Choose one element of `xs` uniformly.
- **`r`** — the generator state.
- **`xs`** — the array to choose from.
- **Returns** a `Draw` whose `value` is the chosen element, or `Null` when `xs` is empty (in which
         case `rng` is returned unadvanced — no draw is consumed).

#### `shuffle`

```lin
val shuffle = <T>(r: Rng, xs: T[]): Draw<T[]>
```

Produce a uniformly-random reordering of `xs` (Fisher–Yates). Non-mutating: `xs` is left
untouched.
- **`r`** — the generator state.
- **`xs`** — the source array.
- **Returns** a `Draw` whose `value` is a fresh shuffled array, plus the advanced `rng`.

#### `shuffleInPlace`

```lin
val shuffleInPlace = <T>(r: Rng, xs: T[]): Rng
```

Shuffle `xs` in place (Fisher–Yates), mutating the given array. Use when you own `xs` and want to
avoid the copy `shuffle` makes.
- **`r`** — the generator state.
- **`xs`** — the array to shuffle in place.
- **Returns** the advanced generator.

#### `sample`

```lin
val sample = <T>(r: Rng, xs: T[], k: Int32): Draw<T[]>
```

Draw `k` elements from `xs` without replacement (each source element appears at most once), in
random order. Non-mutating (partial Fisher–Yates over a copy).
- **`r`** — the generator state.
- **`xs`** — the source array.
- **`k`** — how many to draw; `k >= length(xs)` yields a full shuffle, `k <= 0` yields empty.
- **Returns** a `Draw` whose `value` is the sampled array, plus the advanced `rng`.

### Global convenience API

#### `reseed`

```lin
val reseed = (n: UInt64): Null
```

Reseed the global generator deterministically — the script-level analogue of `seed(n)`. After
`reseed(n)` the global draws replay the same sequence on every run.
- **`n`** — the seed.
- **Returns** null.

#### `next`

```lin
val next = (): Int32
```

Draw the next raw `Int32` from the global generator.
- **Returns** the next raw 32-bit value (may be negative).

#### `int`

```lin
val int = (lo: Int32, hi: Int32): Int32
```

Draw a uniform integer in `[lo, hi)` from the global generator.
- **`lo`** — the inclusive lower bound.
- **`hi`** — the exclusive upper bound.
- **Returns** the drawn integer (`lo` when `hi <= lo`).

#### `real`

```lin
val real = (): Float64
```

Draw a uniform `Float64` in `[0, 1)` from the global generator.
- **Returns** the drawn float in `[0, 1)`.

#### `flip`

```lin
val flip = (): Boolean
```

Draw a coin flip from the global generator.
- **Returns** a uniform `Boolean`.

#### `pick`

```lin
val pick = <T>(xs: T[]): T | Null
```

Choose one element of `xs` from the global generator.
- **`xs`** — the array to choose from.
- **Returns** the chosen element (typed `T | Null`), or `Null` when `xs` is empty.

**Example:**

```lin
pick([10, 20, 30])   // e.g. 20
```

#### `shuffled`

```lin
val shuffled = <T>(xs: T[]): T[]
```

Produce a randomly-ordered copy of `xs` from the global generator (non-mutating).
- **`xs`** — the source array.
- **Returns** a fresh `T[]` with the same elements in random order.

**Example:**

```lin
shuffled([1, 2, 3, 4])   // e.g. [3, 1, 4, 2]
```
