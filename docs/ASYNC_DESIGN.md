# Async / Concurrency — Design & Implementation Plan

Status: **implemented** (Phases 0–8). This document described turning Lin's
concurrency primitives from their synchronous stub into real OS-thread
concurrency (`SPECIFICATION.md` §24); that work has landed. As-built decisions:
ADR-028 (model: copy-by-default RC + catchable faults), ADR-029 (`Shared<T>`),
ADR-030 (`Frozen<T>`) in `docs/DECISIONS.md`; RC-under-threads model in
`MEMORY_MANAGEMENT.md`.

Implemented: real `async`/`await` on OS threads, fault isolation at the thread
boundary (a thunk fault → `Error` at `await`), transfer-by-deep-copy (Option C)
for thunk envs + results, order-preserving `parallel`, real `race`/`timeout`/
`retry`, a bounded `ThreadPool` (`poolAsync`), long-lived `Worker`s
(request/message/close + `onShutdown`), `Shared<T>` (atomic box + RwLock) and
`Frozen<T>` (immortal deep-freeze, lock-free concurrent reads). Guarded by the
full gate + ASan + a TSan leg on the runtime.

Deferred (in the ADRs, not blocking): compile-time *type-level* enforcement for
`Shared<T>` (accessor-only) and `Frozen<T>` (mutation-inference read-only
coercion), each needing a dedicated `Type` variant; the `pool.async` exact
spelling (shipped as `poolAsync`); `pool.serve` multi-threaded HTTP. All runtime
semantics are fully implemented and verified.

The original proposal text follows.

---

## 1. Where we are today

The language **surface** for concurrency is fully specified and largely wired
through the compiler. The **runtime** is a synchronous stub. Concretely:

| Layer | State |
| --- | --- |
| Spec (§24) | Complete: OS threads, `Promise<T \| Error>`, transferable types, fault isolation, combinators, `ThreadPool`, `Worker`. |
| Parser | Done — no new syntax; async is just built-in functions. |
| Type checker | Done — `lin_async`/`pool.async` enforce the `var`-capture ban (`checker/call.rs`) and transferable-return check (`is_definitely_non_transferable`, `checker/helpers.rs`); intrinsic signatures defined (`checker/intrinsics.rs`). |
| IR | Done — `Intrinsic::{Async,Await,Parallel,Race,Timeout,Retry,ThreadPool,Worker,Request,Message,Close}` (`lin-ir/src/ir.rs`, lowered in `lower.rs`). |
| Codegen | **Synchronous** — `Intrinsic::Async` calls the thunk *inline* at the call site then wraps the result (`codegen/intrinsics.rs:249`). `Parallel` runs thunks in a sequential loop. `Race/Timeout/Retry` return their argument unchanged. |
| Runtime (`async_rt.rs`) | **Stub** — `lin_pool_async_*` calls the thunk inline; `LinThreadPool` ignores `n`; `lin_worker_request` runs the handler on the calling thread; `lin_worker_close` is a no-op. Header comment: *"All operations are implemented synchronously for simplicity… semantically correct for non-concurrent programs."* |

So programs that *use* async produce correct results, but with **zero
parallelism** and **no true concurrency semantics** (no overlap, no background
workers, `race`/`timeout` are meaningless, a worker that blocks waiting for a
later message would deadlock the program).

### 1.1 The three hard problems

Making this real is not a runtime swap. Three things block it, in order of
depth:

1. **Runtime errors abort the process.** `lin_panic` and every runtime fault
   (array OOB `array.rs:335`, division by zero, non-exhaustive match) call
   `std::process::exit(1)`. Spec §24.2.2 requires a thunk's runtime error to be
   *caught at the thread boundary* and surfaced as an `Error` value at `await`.
   That is the entire point of `async` being Lin's only fault-isolation
   boundary — and it is fundamentally incompatible with `process::exit`.
   **This is the deepest change** and touches the whole error model, not just
   async.

2. **Reference counting is non-atomic.** Every `refcount += 1 / -= 1` on
   strings/arrays/objects/closures is a plain non-synchronized `u32`
   (`MEMORY_MANAGEMENT.md`; confirmed in `string.rs`, `object.rs`, `array.rs`,
   `memory.rs`). The moment two threads share a heap value and one of them
   retains/releases, that is a data race → use-after-free or leak. Real threads
   require the RC model to become thread-safe.

3. **Codegen evaluates thunks eagerly.** `call_thunk_value` is invoked inline in
   the `Async` lowering (`intrinsics.rs:259`). To run on another thread, codegen
   must instead hand the *unevaluated* closure (fn_ptr + env_ptr) to a runtime
   spawn function. This is a comparatively mechanical change but it is a codegen
   change, not a runtime-only one.

The good news: the spec's design choices were made *anticipating* this. The
`var`-capture ban and the transferable-type restriction (already enforced by the
checker) exist precisely to make threads safe — they guarantee a thunk shares
only immutable, JSON-shaped, cycle-free data with its parent.

---

## 2. Design

### 2.1 Threading model

- **`async(f)`** spawns a real OS thread (`std::thread::spawn`) that runs the
  thunk and stores the result (or caught `Error`) into a shared `LinPromise`.
- **`await(p)`** joins / blocks on that promise until resolved.
- **`ThreadPool(n)`** owns `n` worker threads and an MPMC task queue; `pool.async`
  enqueues work rather than spawning. Plain `async` = spawn-per-call.
- **`Worker`** is a single long-lived thread with an MPSC mailbox; `request`
  blocks for the reply (oneshot back-channel), `message` is fire-and-forget,
  `close` drains, runs `onShutdown`, and joins.

Implementation in `lin-runtime` uses `std::thread`, `std::sync::{Arc, Mutex,
Condvar}` / channels. No async executor, no `tokio` — Lin's model is blocking
OS threads, which matches the spec ("computed on another OS thread").

### 2.2 What may cross a thread boundary (already enforced)

The checker already guarantees a thunk:
- captures **no `var`** bindings (no shared mutable state), and
- returns a **transferable** type (JSON-compatible: scalars, `String`, `T[]`,
  transferable objects — never `Function`, `Iterator`, `Promise`, `Worker`,
  `ThreadPool`).

Workers are the deliberate exception: a worker handler *may* close over `var`
(spec §24.6.4) because the worker thread processes messages sequentially — the
state is confined to one thread, never concurrently accessed.

**Consequence for RC (see 2.3):** what actually gets shared across threads is
(a) the closure env of a spawned thunk — immutable `val` captures — and (b) the
transferable result value handed back through the promise. Both are
heap-refcounted, so both touch the RC model.

### 2.3 Reference counting under threads — options

This is the load-bearing decision. Four options, with the recommendation last.

**Option A — Global atomic RC.** Make every refcount an `AtomicU32` and every
retain/release a `fetch_add`/`fetch_sub`. Simple and always-correct.
*Cost:* atomic ops on the single-threaded hot path too — measurable slowdown
(typically 5–20% on RC-heavy code; e.g. Swift pays this). Pessimizes the 99% of
code that never touches a thread.

**Option B — Biased / deferred RC.** Each object is "owned" by one thread and
uses cheap non-atomic ops there; only cross-thread access pays atomics (à la
Swift's earlier biased RC, or RubyRC). *Cost:* significant runtime complexity,
an owner-thread field per object, and subtle correctness work. Too much for v1.

**Option C — Transfer by deep copy ("share nothing").** When a value crosses a
thread boundary (into a thunk's env, or back through a promise), **deep-copy**
it so each thread owns a private, disjoint object graph. RC stays non-atomic
because nothing is ever shared. *Cost:* copying transferable data at the
boundary. *Benefit:* the single-threaded hot path is completely untouched, and
it matches the spec's mental model exactly — "transferable" values are
JSON-ish, acyclic, and finite, so a deep copy is well-defined and bounded.
This is essentially the actor/Erlang and Web Worker `postMessage` model.

**Option D — Atomic RC only for "shared" values, decided dynamically.** A
per-object flag flips to "shared/atomic" when a value crosses a boundary at
runtime; ops branch on the flag. *Cost:* a branch + flag bit on every RC op, and
— the killer — the **flip itself is a race**: every pre-existing reference
(including the parent thread's) must switch to atomic mode with a correct
happens-before barrier before any concurrent access, or you get mixed
atomic/non-atomic access to one location, which is UB. Easy to get wrong in
ways that only surface under load. Critically, atomic RC makes the **refcount**
safe but does **nothing** for the object's *contents* (`len`, `cap`, the backing
buffer) — so it does not, by itself, give safe shared *mutable* data anyway.

**Recommendation: Option C by default, plus two explicit, opt-in shared types —
`Shared<T>` for shared *mutable* state (§2.3.1) and `Frozen<T>` for shared
*read-only* state (§2.3.2).** Ordinary values are copy-by-default (C); a program
that wants one object visible to many threads opts into the right box and pays
its cost only there. `Shared<T>` = atomic RC + `RwLock` (mutable, accessor-only).
`Frozen<T>` = immortal deep-frozen graph (immutable, zero-copy, lock-free,
readable through the plain type via mutation-inference coercion) — the answer to
"read-only should *just work* fast without wrapping every access."

Rationale:
- It keeps the **single-threaded fast path at zero cost** — the thing we have
  just spent effort optimizing (literal interning, nounwind, RC elision) is not
  regressed for the overwhelmingly common non-threaded program. The atomic-RC +
  lock machinery attaches *only* to `Shared` boxes, which the type system
  identifies statically.
- The set of values that cross a boundary *by copy* is **exactly the
  transferable types**, which are already acyclic and JSON-shaped — deep copy is
  trivial and total. We cannot be handed a `Function`/`Iterator`/cyclic graph at
  a boundary (checker forbids it).
- It composes with the **immortal interned strings** just merged: an immortal
  literal can be referenced from a copy without issue (never mutated / freed).
- Workers' `var` state never crosses a boundary (it's confined to the worker
  thread), so this doesn't conflict with §24.6.4.
- Making `Shared<T>` an explicit *type* dissolves Option D's flip hazard: a value
  is shared-or-not **statically**, so there is no runtime flip, no mixed-mode
  access, and no barrier to get wrong.

The one subtlety for the copy path: the **closure env of a spawned thunk** must
also be transferred by copy. Since the thunk may capture only `val`s (no `var`)
and the captured values are transferable-or-functions-over-transferables, the
env is copyable. Captured *functions* (allowed by §24.2.1 if they close over no
`var`) need their own env deep-copied transitively. This is the trickiest part
of the copy path and needs care (see Phase 3 risks).

### 2.3.1 `Shared<T>` — opt-in shared mutable state

For the case copy-by-default handles badly — many threads sharing one large
and/or mutable structure (a lookup table, a cache, an accumulator) — Lin
provides an explicit shared box. This deliberately supersedes the earlier
"share-nothing only" stance (the TODO line "share-nothing upheld over a Mutex
primitive" is hereby revised); see §2.3.4 for how it relates to `Worker`, and
§2.3.2 for `Frozen<T>`, its read-only sibling.

**Surface:**

```txt
shared:   <T>(T) => Shared<T>
get:      <T>(Shared<T>) => T                 // read-lock, deep-copy out a snapshot
set:      <T>(Shared<T>, T) => Null           // write-lock, copy in
withLock: <T, R>(Shared<T>, (T) => R) => R    // write-lock held across f, copy out result
```

```txt
val s = shared([4, 5, 6])                 // Shared<Int32[]>

val snap = s.get()                        // snapshot copy; many get()s run concurrently
s.set([7, 8, 9])                          // replace the value wholesale

withLock(s, arr => push(arr, 7))          // atomic read-modify-write, in place
val n = withLock(s, arr => length(arr))   // read just one derived value out
```

**Read vs. write is chosen by *which operation you call*, not by policing a
closure body — so there is nothing to "enforce."** This is the answer to the
natural question "how do we make a read-only lock read-only?": we don't hand the
caller a borrow they must promise not to mutate (Lin has no immutability
qualifier to enforce that). Instead `get()` returns a **copy** — read-only-ness
is free because the caller holds a private value, not a reference into the box.
The three operations map onto a reader-writer lock:

| op | lock mode | hands you | runs concurrently with |
| --- | --- | --- | --- |
| `get(s)` | **read** (shared) | a deep copy (snapshot) | other `get`s |
| `set(s, v)` | write (exclusive) | — | nothing |
| `withLock(s, f)` | write (exclusive) | inner value, mutable, in place | nothing |

So a read-heavy lookup table lets all reader threads run in parallel; only
writers serialize. The box is backed by an `RwLock` (not a plain `Mutex`).

**`get`/`set` vs `withLock` — atomicity.** `get` then later `set` is **not**
atomic across the gap: the lock is released between them, so two threads that
each `get` → modify → `set` can lose an update (last-writer-wins). That is the
right tool for *snapshot-read, do slow work unlocked, publish result* — you do
**not** hold the lock during the work:

```txt
val snap = s.get()    // lock held only for the copy
// ... long work / IO / await — NO lock held ...
s.set(result)         // lock held only for the store
```

For an atomic read-modify-write (a counter, "increment if present"), you must
hold the lock across the read and write — that is exactly `withLock`, whose lock
spans the whole of `f`:

```txt
withLock(s, count => count + 1)   // atomic; keep f short, no IO inside
```

Rule of thumb: `withLock` when the update must be atomic (hold it briefly);
`get`/`set` when you want to read a snapshot, work without holding a lock, and
publish — accepting last-writer-wins.

> Deliberately **not** added: a closure-form read lock (`readWithLock(s, f)`).
> It cannot be made sound — `f` could mutate the borrowed value and there is no
> immutability qualifier to forbid it. `get()` (copy-out) is the sound read.
> Also **not** added: a manual `lock()` / `unlock()` (or `withLock()` returning a
> handle you later `write()` back) pair — it reintroduces forgot-to-unlock
> deadlocks, lock-held-across-IO, and self-deadlock on re-entry, all of which the
> scoped forms prevent. `get`/`set` is the safe expression of "read, work
> unlocked, write back."

**Three enforced safety rules — together they make "mutate a shared value without
the lock" unrepresentable, not merely detected:**

1. **`Shared<T>` is opaque; `get`/`set`/`withLock` are the *only* accessors.**
   There is no other operation on a `Shared<T>` (plus `shared`/RC). You cannot
   index it, call array/object ops on it, or auto-unwrap it — those are
   **compile-time type errors** (`push(s, 7)` fails: `push` wants `Int32[]`, got
   `Shared<Int32[]>`). The unlocked inner value is simply not reachable except
   through these three, each of which takes the appropriate lock for its whole
   duration (scoped/RAII — no manual unlock, no "forgot to unlock"). This is the
   same philosophy as the rest of Lin's safety story (safe bracket access,
   value-based errors): make the footgun *impossible to hold* rather than
   diagnosed after the fact.

2. **Every value leaving the box is copied out; every value entering is copied
   in.** `get` deep-copies the snapshot it returns; `withLock` deep-copies
   whatever `f` returns; `set`/`shared` deep-copy the value they store. So no
   live reference into (or out of) the inner object can escape the lock and be
   touched unsynchronized. This closes the only loophole rule 1 would otherwise
   have:

   ```txt
   val leaked = withLock(s, arr => arr)  // returns a COPY, not the inner array
   push(leaked, 7)                       // harmless — mutates the copy, not s
   ```

3. **`get`/`set` are individually atomic, but not atomic *across* a get→set
   gap.** Each takes its lock for its own duration; the lock is not held between
   them. Code needing an atomic read-modify-write must use `withLock` (rule
   above / the atomicity note). This isn't a soundness rule (no data race either
   way) but a correctness one — it's the lost-update hazard, documented so it's a
   choice, not a surprise.

**Runtime representation.** `Shared<T>` box = **atomic** refcount + an `RwLock` +
the inner value. The box's own refcount is atomic (it's the thing shared across
threads); `get` takes the read lock, `set`/`withLock` the write lock. The
**inner** object graph keeps ordinary **non-atomic** RC, because it is only ever
reachable while a lock is held — all access is serialized (and concurrent
`get`s only *read*, never mutating the inner RC, since they copy out), so a
non-atomic count is safe there. Cost model: atomics only on `Shared`-box
retain/release, an `RwLock` acquire/release per op, and a copy in/out. The
"`withLock`, mutate in place, return a scalar or nothing" pattern is cheap;
`get` on a large structure or `withLock(s, arr => bigDerivedArray)` pays to copy
the big value.

**Nesting / boundary rule.** When the copy path (a thread boundary, or `shared`'s
own copy-in) encounters a `Shared` box embedded in a larger value, it does **not**
deep-copy *through* it — it bumps the box's atomic refcount and shares the box.
The `Shared` box is the marker that means "stop copying, start sharing." This is
the one rule that must be specified precisely; everything else falls out of it.

**Constraints.** `shared(v)` requires `v` transferable (JSON-shaped, acyclic) —
same rule as crossing a thread boundary; `shared(aFunction)`/`shared(anIterator)`
is a compile-time error. `Shared<T>` makes reference cycles reachable (two boxes
referencing each other) and Lin's RC has no cycle collector (ADR-024) — document
the hazard. Any lock primitive reintroduces **deadlock** potential, and Lin has
no cancellation (§24.4 `timeout` only "abandons"); scoped `withLock` plus a
documented no-reentrancy / lock-ordering rule mitigates but does not remove it.

### 2.3.2 `Frozen<T>` — opt-in shared **read-only** state (zero-copy, lock-free)

`Shared<T>` handles shared *mutable* state, but its reads either serialize
(`withLock`) or copy (`get`). The dominant real case is different: a large
structure built once and **only read** by many threads — a timetable, a routing
table, a config, a parsed grammar — where you want concurrent reads to be
zero-copy, lock-free, and written in ordinary syntax. That is `Frozen<T>`.

**Surface — one word, at creation only:**

```txt
frozen: <T>(T) => Frozen<T>
```

```txt
val timetable = frozen(loadTimetable())     // Frozen<Timetable> — the only ceremony

val results = parallel(
  journeys.map(j => () => planJourney(timetable, j))   // shared by reference, not copied
)
```

**Reads use normal syntax; the type is invisible to readers (the key ergonomic).**
A `Frozen<T>` is **not** threaded through every signature. The journey planner is
written against the plain type and is oblivious to freezing:

```txt
val planJourney = (tt: Timetable, j: Journey): Route => ...   // plain Timetable
plan(timetable, journey)   // ✓ a Frozen<Timetable> is accepted here
```

This works via a **read-only coercion rule**, *not* blanket subtyping. Note
`Frozen<T> <: T` would be **unsound** — `T` permits mutation, so substituting a
frozen value into a slot whose function does `push`/`set` on it would write to
immutable shared data. The variance is the other way (`T <: Frozen<T>` in spirit
— a mutable thing can stand in where read-only is wanted, not vice-versa). So the
actual rule is gated on a **mutation-inference** pass:

> A `Frozen<T>` is accepted in a `T` parameter slot **iff the callee does not
> mutate that parameter.** The checker infers per-function, per-parameter "does
> this mutate?" bottom-up across the call graph (mutation of a param passed on
> to a callee propagates up), and records a "mutates param *i*" bit in each
> function's `ModuleSignature` (reusing the existing signature cache). Unknown /
> FFI calls on a param ⇒ conservatively "mutates."

If `planJourney` *did* mutate `tt`, passing the frozen value is a clean
**compile error** ("planJourney mutates parameter `tt`; cannot pass a Frozen
value"). So read-only "just works"; the only thing forbidden is silently
mutating frozen data. Indexing a `Frozen` yields a `Frozen` sub-value (or an
immediate scalar), so `timetable["WAT"]["routes"][3]` is plain pointer-chasing
into the shared graph — no lock, no copy.

**Why it's safe without atomic RC — the immortality trick.** The trap: a
read-only function is compiled once against `Timetable`, so its internal
`retain`/`release` on the parameter are **non-atomic**. Run it on 1000 threads
sharing one frozen value and those refcount writes would race — even though the
*contents* are never written. The fix is the mechanism we already shipped for
interned strings (`IMMORTAL_RC`): **`frozen(v)` produces an immortal, deep-frozen
graph** — every node marked immortal (saturated refcount) and immutable. Then:

- contents are immutable → concurrent **reads** are safe;
- the refcount is **never written** (retain/release are guarded no-ops that only
  *read* the sentinel) → concurrent reads of the count are not a race (a race
  needs a writer);
- therefore the read-only function's existing **non-atomic** RC code runs
  correctly on a shared frozen value **with no recompilation, no lock, and no
  atomic ops**.

This is the literal-interning trick generalized from one string to a whole
object graph, and it is exactly what makes "read-only just works efficiently."

**Cost & constraints.**
- **Immortal ⇒ never freed.** Ideal for load-once, program-lifetime reference
  data (one O(size) deep-freeze at startup, then free for the program's life). A
  `frozen()` value created-and-discarded in a loop is a **leak** — `frozen` is
  for long-lived data. (Ephemeral frozen values would need atomic RC +
  monomorphizing readers; explicitly out of scope for v1.)
- **`frozen(v)` is a deep, one-time operation** — transitively seals the graph;
  `v` must be transferable/acyclic (same rule as `shared`).
- **Mutation inference is interprocedural** — tractable (bottom-up, cached in
  signatures; the checker already analyses thunk captures for the `var` ban) but
  it is real analysis and must stay conservative.
- A frozen graph is acyclic and immutable, so unlike `Shared<T>` it adds **no
  deadlock and no new cycle hazard**.

**Future: auto-freeze.** The fully-automatic version — infer "this captured
value is only read by the thunk *and* not mutated by the parent after spawn," and
auto-`frozen` it at the boundary — is reachable later. The parent-side
non-mutation/no-escape proof is the hard half. Ship explicit `frozen()` first so
the intent is a *checked fact*, not an inferred guess that is a silent data race
if the inference is ever wrong; layer inference on top once proven.

### 2.3.3 Considered and rejected: copy-on-write (COW)

A tempting alternative to "copy at the boundary" is to share everything by
reference and copy lazily on first write (à la Swift arrays / V8). Rejected for
Lin, for two reasons specific to this language:

1. **It breaks in-place mutation semantics.** Lin mutates *through* containers:
   `push(data["items"], 4)`. COW would copy the inner array on the `push`, but
   that copy is a temporary never stored back into `data` — so the mutation
   vanishes, and worse, the *same expression* would return different results
   depending on whether the value had crossed a thread boundary
   (sharing-history-dependent semantics). Swift avoids this because its COW
   writes back through a mutable binding; Lin's `push(container[key], v)` has no
   binding to write back through.
2. **It taxes the single-threaded hot path.** COW needs a "is this shared? then
   copy" check and an **atomic** refcount on every potentially-shared value —
   precisely Option D's per-op cost, landing on the non-threaded code we just
   optimized (interning / nounwind / RC elision), plus the shared-flag flip
   hazard.

`Frozen<T>` (read-only) + `Shared<T>` (mutable) deliver COW's actual goal —
**don't copy big shared data into every thread** — as explicit, statically-typed
choices, with zero hot-path cost and no semantic surprises.

### 2.3.4 `Shared<T>` vs `Worker`

Both give safe shared mutable state; they are different ergonomics and we keep
both:

- **`Worker`** — state is *owned* by one thread; other threads **send messages**
  and the owner serializes access. Best for stateful services, when the state
  has behavior, or when you want a queue. No shared memory at all (messages are
  copied).
- **`Shared<T>`** — state is a *passive* structure many threads **lock and
  touch** directly. Best for a plain shared table/counter/cache where spinning
  up an owner thread + message protocol is overkill. Its `RwLock` also gives
  **concurrent reads** (`get`), which a single-owner `Worker` cannot — every
  request to a worker serializes on that one thread. So a read-heavy shared
  lookup table favours `Shared<T>`; a write-heavy or behaviour-rich one favours
  `Worker`.

Rule of thumb: reach for `Worker` when the state has logic or a lifecycle;
`Shared<T>` when it's just data that several threads must read/update.

### 2.4 Fault isolation (problem #1)

Spec §24.2.2: a runtime error inside a thunk becomes an `Error` value at
`await`, not a process abort. Required mechanism:

1. **A catch boundary at the thread entry.** The spawned thread runs the thunk
   inside a Rust `std::panic::catch_unwind`. Runtime faults must therefore
   **panic instead of `process::exit`** *when running on an async thread*.
2. **Re-route runtime faults.** `lin_panic` and the inline fault sites
   (array OOB, div-by-zero, non-exhaustive match) need a thread-local "are we
   inside an async boundary?" flag. Inside a boundary → `panic!`
   (unwinds to the `catch_unwind`, becomes an `Error`); outside → keep current
   `process::exit(1)` (uncatchable, per §20.1).
3. **`nounwind` interaction.** We just marked user functions `nounwind` (sound
   because they currently never unwind). If runtime faults inside async thunks
   begin to unwind through user frames, `nounwind` becomes **unsound** for the
   functions on an async call path. Options: (a) compile the thunk-reachable
   functions *without* `nounwind`; (b) don't unwind through Lin frames at all —
   instead have the fault site set a thread-local error + `longjmp`/early-return
   to the boundary; (c) make the panic landing pad the Rust runtime frame only.
   **This must be resolved before or together with making faults catchable** —
   it is a direct dependency created by the optimization work.
4. **`build = panic=abort`?** The runtime is currently `panic=unwind` (we relied
   on that to *not* mark runtime decls `nounwind`). `catch_unwind` needs
   `panic=unwind` — consistent. But the compiled *user* binary's panic behavior
   and the Rust runtime's must be reconciled; verify the linked binary unwinds
   correctly across the Rust/LLVM boundary.

This is the single most invasive piece and the main reason async is a project,
not a patch.

### 2.5 Codegen changes

- **`Async`**: stop calling `call_thunk_value` inline. Instead extract
  `(fn_ptr, env_ptr)` from the closure (as `Worker` already does at
  `intrinsics.rs:347`) and pass them to a new `lin_async_spawn(fn_ptr, env_ptr,
  has_env) -> *LinPromise`. The runtime owns the spawn + the env copy.
- **`Parallel`**: spawn all thunks, then join all — replace the sequential loop
  with spawn-loop + join-loop (or delegate entirely to a runtime
  `lin_parallel(array) -> array`).
- **`Race/Timeout/Retry`**: real implementations in the runtime; codegen just
  forwards the promise(s).
- **Promise combinators (`map`)**: `Promise.map` needs the transform closure to
  run when the promise resolves — either eagerly on `await` or on a follow-on
  thread. Simplest v1: store the transform with the promise, apply on `await`.

### 2.6 Runtime data structures (`async_rt.rs` rewrite)

- `LinPromise`: `Arc<(Mutex<Option<TaggedVal-or-Error>>, Condvar)>` + a
  `JoinHandle`. `await` locks, waits on the condvar until `Some`, returns the
  (deep-copied-into-caller) value.
- `LinThreadPool`: `n` `JoinHandle`s + a shared work queue (`Arc<Mutex<VecDeque>>`
  + `Condvar`, or `crossbeam-channel` if we accept the dep). Tasks carry
  `(fn_ptr, copied-env, result-slot)`.
- `LinWorker`: one `JoinHandle` + an MPSC `Sender<Message>`; `request` includes
  a oneshot reply channel; `close` sends a shutdown sentinel and joins.
- **Transfer/copy fns**: `lin_transfer_clone(TaggedVal) -> TaggedVal` that deep-
  copies a transferable graph (and a parallel one for closure envs). Must reject
  / never-receive non-transferable tags (checker guarantees the static cases;
  runtime guards the dynamic `AnyVal` case per spec §24.2).

### 2.7 `print` ordering (§24.7)

Spec requires line-atomic `print` across threads. Wrap stdout writes in a global
`Mutex` (or use `std::io::Stdout`'s internal lock and write whole lines). Cheap,
do it as part of the runtime rewrite.

---

## 3. Phased implementation plan

Each phase is independently testable and merges on green. Phases are ordered so
that the riskiest, most cross-cutting work (fault isolation) is de-risked early
with a spike but landed in the middle once the threading scaffolding exists.

### Phase 0 — Spike & decision lock-in (no merge)
- Prototype `std::thread::spawn` + `catch_unwind` around a single Lin thunk in a
  throwaway branch. Confirm a Lin runtime fault can be turned into a caught
  `Error` and that unwinding crosses the LLVM/Rust boundary cleanly.
- Lock in the RC strategy: **Option C (copy) by default + opt-in `Shared<T>`**
  (§2.3 / §2.3.1). Still measure atomic-RC (Option A) cost on the existing
  benchmark suite so the `Shared`-box atomic-count cost is quantified, not
  assumed, and so we know the price had we gone all-atomic.
- Output: a short decision record (new ADR) fixing the RC model
  (C + `Shared<T>`) and the fault-isolation mechanism.

### Phase 1 — Fault isolation foundation
- Introduce a thread-local "async boundary depth" flag in the runtime.
- Make `lin_panic` and each inline fault site branch on it: panic-unwind inside
  a boundary, `process::exit` outside.
- Resolve the `nounwind` soundness issue (2.4 item 3) — likely: do not mark
  functions reachable from async thunks `nounwind`, or switch the fault
  mechanism to non-unwinding. Re-run the **ASan + full gate**.
- No user-visible behavior change yet (no thread is spawned); this is pure
  plumbing, verifiable by a Rust-level unit test that calls a faulting thunk
  through the boundary.

### Phase 2 — Real `async` / `await` (spawn-per-call)
- New runtime `lin_async_spawn` + real `LinPromise` (thread + condvar).
- Codegen `Async` hands off the unevaluated closure; `Await` joins.
- Implement `lin_transfer_clone` for transferable results + closure envs
  (Option C).
- Tests: the existing `stdlib/async.test.lin` must still pass **and** new tests
  that prove actual parallelism (e.g. two thunks that each sleep 100ms complete
  in ~100ms wall-clock, not 200ms) and fault isolation (a thunk that divides by
  zero yields an `Error` at `await`, program continues).
- ASan + a `--features thread-sanitizer`/TSan leg if feasible (TSan is the right
  tool for the RC-race class; add it to CI for the runtime).

### Phase 3 — `parallel`, combinators
- `parallel` = spawn-all + join-all, order-preserving.
- `race`, `timeout`, `retry`, `Promise.map` real implementations.
- `timeout` abandons (does not cancel) the slow thread per spec.
- Risk focus: transitive deep-copy of closure envs that themselves capture
  functions (2.3 subtlety). Add targeted tests.

### Phase 4 — `ThreadPool`
- Bounded pool + work queue; `pool.async` single + array overloads.
- `pool.serve` for multi-threaded HTTP (ties into §25.5 — coordinate with the
  http runtime).

### Phase 5 — `Worker`
- Long-lived thread + mailbox; `request` (blocking, oneshot reply), `message`
  (fire-and-forget), `close` (drain + `onShutdown` + join).
- This is where `var`-capturing handlers are legal (§24.6.4) — verify the state
  stays thread-confined (no copy of the worker's own `var` env; it lives on the
  worker thread for the worker's lifetime).
- Worker fault kills the worker; in-flight `request` surfaces the diagnostic;
  later sends to a dead worker are runtime errors (§24.6.5).

### Phase 6 — `Shared<T>` (opt-in shared mutable state)
This is a genuinely new language feature (not in the current spec) — comparable
in size to adding Workers — so it lands after the spec'd primitives work.
- New opaque type `Shared<T>` + built-ins `shared`/`get`/`set`/`withLock`
  (§2.3.1): parser (none — built-in functions), checker (the type is opaque:
  reject every op on `Shared<T>` except the four accessors; enforce `T`
  transferable in `shared(v)`/`set`), IR, codegen, runtime (atomic-refcount box +
  `RwLock` + inner value; `get` = read lock, `set`/`withLock` = write lock).
- Implement the safety rules: the four accessors are the only ops (type system);
  copy-in on `shared`/`set`, copy-out on `get`/`withLock` (runtime). Add the
  nesting/boundary rule: the copy path shares a `Shared` box by atomic-refcount
  bump, never deep-copies through it.
- Spec amendment: add a `Shared<T>` section to `SPECIFICATION.md` §24 and the
  `Worker`-vs-`Shared` guidance (§2.3.4); revise the TODO "share-nothing upheld"
  line. New ADR.
- Tests: TSan stress (N threads: concurrent `get`s + serialized `withLock`
  mutations on one box); the escape-via-return loophole returns a copy (mutating
  it doesn't affect the box); `push(s, x)` without an accessor is a compile-time
  error (negative test); `withLock` increment is atomic under contention while a
  `get`/`set` race exhibits lost updates (documents, not asserts-against, the
  caveat).

### Phase 7 — `Frozen<T>` (opt-in shared read-only state)
The other new feature (§2.3.2). Reuses the immortal-RC machinery from the merged
string-interning work, generalized to whole graphs.
- Mutation-inference pass in `lin-check`: per-function, per-parameter "mutates
  this param?" bit, computed bottom-up across the call graph, cached in
  `ModuleSignature`; conservative on unknown/FFI calls. This is the prerequisite
  and the bulk of the work — useful on its own (also informs borrow/RC opts).
- New type `Frozen<T>` + built-in `frozen` (§2.3.2): checker (read-only coercion
  rule — accept `Frozen<T>` in a `T` slot iff the callee doesn't mutate it;
  reject mutation of a `Frozen` with a clear diagnostic; `frozen(v)` requires `v`
  transferable/acyclic), IR, codegen (indexing a `Frozen` yields `Frozen`),
  runtime (`lin_freeze` = deep, transitive immortal+immutable seal of the graph).
- Reuse `IMMORTAL_RC`: frozen nodes are immortal, so existing non-atomic
  retain/release become guarded no-ops on them — read-only functions run on
  shared frozen data **with no recompilation, no lock, no atomics**.
- Spec amendment: `Frozen<T>` section in §24; document the immortal⇒never-freed
  lifetime (load-once data, not loop-allocated). New ADR.
- Tests: the timetable pattern (one `frozen` graph, N threads reading via the
  plain-typed planner) under TSan — zero races, zero copies; passing a `Frozen`
  to a param that mutates it is a compile error (negative test); a `frozen`
  value read through deep index chains returns correct values across threads.

### Phase 8 — Hardening & docs
- TSan in CI on the runtime; stress tests (high fan-out, many short tasks,
  worker churn).
- Update `SPECIFICATION.md` only if reality forces a clarification; update
  `MEMORY_MANAGEMENT.md` with the chosen RC-under-threads model; new ADRs.
- Revisit `async_rt.rs`'s "synchronous" header comment (delete it).

---

## 4. Risks & open questions

- **RC model.** Copy-by-default (C) is safe and keeps the single-threaded path
  free, but copies large transferable results at the boundary. `Shared<T>`
  (§2.3.1) is the escape hatch for the big-shared-data case, so we are *not*
  forced into all-atomic RC later. Phase 0's benchmark still quantifies what
  all-atomic would have cost.
- **`Shared<T>` / `Frozen<T>` are new features, not a runtime swap.** Each adds a
  type + built-ins across checker/codegen/runtime (Phases 6–7) and amends the
  spec. `Shared<T>` soundness rests on two rules holding together (accessor-only
  + copy in/out); a gap in either reopens the data race; deadlock and RC-cycle
  hazards (§2.3.1) come with it.
- **`Frozen<T>` leans on two analyses.** (a) **Mutation inference** (the
  read-only coercion that lets readers use the plain type) — interprocedural,
  must stay conservative, and is the bulk of Phase 7; getting "doesn't mutate"
  wrong in the *unsound* direction would let a frozen value reach a mutating
  param. (b) The **immortal⇒never-freed** lifetime — perfect for load-once data,
  a leak for loop-allocated frozen values; `frozen` is documented as
  long-lived-data only.
- **`nounwind` vs. catchable faults** (2.4.3) is a real, already-created
  dependency from the optimization work. Must be settled in Phase 1.
- **Deadlocks / blocking semantics**: `await`, `request`, and `close` all block.
  A worker that `request`s itself, or a cyclic worker topology, can deadlock.
  Spec has no cancellation in v1 (§24.4 `timeout` "abandons"); document the
  hazards.
- **Determinism of tests**: thread timing is nondeterministic. Lean on
  sleep-based wall-clock assertions sparingly; prefer result-correctness +
  TSan/ASan for the race class.
- **`print` interleaving** beyond line-atomicity (e.g. a multi-line report from
  one thread) is explicitly *not* guaranteed by the spec — don't over-engineer.
- **HTTP server** (`pool.serve`, §25.5) is downstream of Phase 4; the current
  http runtime may assume single-threaded — audit before Phase 4.

---

## 5. Summary

The async surface is done and correct-by-construction guards (`var`-capture ban,
transferable-only) are already enforced. Turning the stub into real concurrency
is gated on three things, deepest first: **(1) catchable faults at the thread
boundary** (incompatible with today's `process::exit`, and entangled with the
recent `nounwind` work), **(2) a thread-safe RC story** — *transfer by deep copy*
by default to keep the single-threaded hot path free, plus two opt-in shared
types: `Shared<T>` (opaque, accessor-only, copy in/out, `RwLock`) for shared
*mutable* state and `Frozen<T>` (immortal deep-frozen graph, zero-copy, lock-free,
read through the plain type via mutation-inference coercion) for shared
*read-only* state — and **(3) codegen handing thunks off unevaluated**. The plan
lands these in eight verifiable phases: a spike that locks the model decisions
with measurements, the spec'd primitives (`async`/`await`/`parallel`/combinators/
`ThreadPool`/`Worker`), then `Shared<T>` and `Frozen<T>` as new features, then
hardening. COW was considered and rejected (§2.3.3).
