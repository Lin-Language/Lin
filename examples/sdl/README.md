# sdl — an SDL3 "game" in pure Lin (two FFI modes + 2D vector/matrix math + concurrency)

A single, coherent graphics demo that pulls together most of Lin's systems story:

- **Two FFI modes.** It links the **real, vendored SDL3 3.4.10 shared library** (`libs/libSDL3.so`)
  *and* a **static archive you compile from C source** (`clib/libmathlib.a`, built from
  `clib/mathlib.c`) — both via `import foreign`.
- **Pure 2D vector/matrix math drives the animation.** Positions and velocities are `Vec2 {x, y}`
  records; the ball's motion is `physics.stepBall` (reflecting off the walls) with its velocity
  curved each frame by a `matrix.rotation2D` rotation matrix; the AI agent steers by the vector
  `heading` (atan2) to its goal, rotated through a matrix.
- **Share-nothing concurrency.** `ai_worker.lin` offloads the pure planning step to an `async`
  worker each frame — plain data in, plain data out, no SDL handle ever crossing the boundary.
- **Everything pure is unit tested** (`vector`, `matrix`, `physics`, `agent`, `mathffi`), so the
  SDL driver files stay thin and the logic underneath has coverage.

## The two demos

- **`bounce.lin`** — a bouncing ball. Opens a window (a `char*` title marshalled with `withCstr`, a
  window handle carried as a `Ptr`), creates a renderer, then runs a tail-recursive frame loop over
  a **fixed frame count**: drain events, clear, advance the ball with `physics.stepBall` (the ball
  is an immutable `Ball {pos, vel}` of `Vec2`), build an `SDL_FRect` in a 16-byte buffer with four
  `pokeF32`, set the colour, fill, present. Each frame the velocity is rotated a little by a
  rotation matrix (so the ball curves), and the final distance from the ball to the playfield
  centre is computed in C via `mathffi.cDistance`.
- **`ai_worker.lin`** — the same SDL main-thread loop **plus** an `async` worker. Each frame a plain
  `World {agent, goal}` snapshot is deep-copied into an `async` thunk that runs `agent.planStep`
  (heading via `atan2`, rotate forward vector with a rotation matrix, snap to a grid step, stop when
  the C distance to the goal is ~0). The resulting `Vec2` is deep-copied back and the agent is drawn
  on the main thread.

## Module layout

Pure, unit-tested modules (the testable core extracted from the SDL drivers, like the `processes`
example extracts `runTask`):

- **`vector.lin`** — `Vec2 {x, y}` and 2D vector ops (`add`, `sub`, `scale`, `dot`, `magnitude`,
  `angleBetween`, `heading`). Tested in `vector.test.lin`.
- **`matrix.lin`** — 3x3 matrix math over a flat `Float64[]` (`mat3`, `matMul`, `rotation2D`,
  `applyToVec` mapping a `Vec2` through the matrix). Tested in `matrix.test.lin`.
- **`physics.lin`** — `Ball {pos, vel}` and `stepBall`/`reflect1d`, the ball-physics core extracted
  from `bounce.lin`. Tested in `physics.test.lin`.
- **`agent.lin`** — `World {agent, goal}` and `planStep`/`distanceToGoal`, the agent-planning core
  extracted from `ai_worker.lin` (uses vector heading, a rotation matrix, and the C distance).
  Tested in `agent.test.lin`.
- **`mathffi.lin`** — thin Lin wrappers (`cMagnitude`, `cDistance`, `cClamp`, `cSquare`, `cAdd`,
  `cGcd`) over the C functions in `clib/libmathlib.a`. Round-trip tested in `mathffi.test.lin`.

SDL driver files (not unit-testable — they need the real `.so` + a headless env, and are covered by
the repo's Rust integration tests):

- **`bounce.lin`**, **`ai_worker.lin`** — the two demos above.
- **`constants.lin`** — shared SDL constants + `SDL_FRect`/`SDL_Event`/`SDL_Surface` byte-offsets
  and the XRGB8888 pixel-channel offsets (`export val`).

Vendored / committed C artefacts:

- **`libs/libSDL3.so.0.4.10`** — the real SDL3 3.4.10 shared library (~2.9 MB).
- **`libs/libSDL3.so.0`**, **`libs/libSDL3.so`** — the soname symlink chain (committed).
- **`clib/mathlib.c`** — the hand-written C math library source (`add`, `square`, `clampf`,
  `magnitude2`, `gcd`).
- **`clib/libmathlib.a`** — the committed compiled static archive `lin` links against.

## FFI mode 1 — a vendored shared library (SDL3), headless via the dummy driver

`libs/` contains the **real** `libSDL3.so.0.4.10` plus the soname symlink chain
`libSDL3.so -> libSDL3.so.0 -> libSDL3.so.0.4.10`. All three are committed: git tracks symlinks,
the library's soname is `libSDL3.so.0` (so the produced binary's `NEEDED` entry is `libSDL3.so.0`),
and the `$ORIGIN`-rpath loader resolves that soname through the chain at runtime. The demos import
it by relative path: `import foreign "examples/sdl/libs/libSDL3.so"`.

There is **no display in CI**, so the demos run **headless**: set `SDL_VIDEODRIVER=dummy` and SDL3
selects the dummy video driver plus the **software renderer** — real rasterisation, no GPU or
display needed. (On a machine with a display, do *not* set the variable — or set
`SDL_VIDEODRIVER=x11`/`wayland` — to see actual graphics in a window.)

Real headless SDL3 emits **no synthetic `SDL_EVENT_QUIT`**, so a poll-to-quit loop would hang
forever. The demos therefore run a **fixed frame count** (60) and self-terminate. They still *pump*
the event queue each frame (draining and discarding), they just don't depend on it to stop.

## FFI mode 2 — a static archive you compile from C (`libmathlib.a`)

`clib/mathlib.c` is a small C math library (`add`, `square`, `clampf`, `magnitude2`, `gcd`),
compiled to the committed static archive `clib/libmathlib.a`. The C ABI type mapping is
`int32_t` ↔ `Int32`, `double` ↔ `Float64`. `mathffi.lin` declares the foreign block and exposes
clean Lin wrappers; the demos call `cDistance` (which routes to C `magnitude2` → libm `sqrt`) to
measure real distances. Rebuild the archive after editing `mathlib.c`:

```sh
cc -c examples/sdl/clib/mathlib.c -o examples/sdl/clib/mathlib.o
ar rcs examples/sdl/clib/libmathlib.a examples/sdl/clib/mathlib.o && rm examples/sdl/clib/mathlib.o
```

### File-local foreign bindings (important)

`import foreign` bindings are **file-local** — the library is linked only for the file that declares
the block, and the bindings **cannot be re-exported** from a wrapper module. So:

- `bounce.lin` and `ai_worker.lin` each declare their **own** `import foreign "…/libSDL3.so"` block.
- `mathffi.lin` declares the `…/clib/libmathlib.a` block and wraps it. But because the linkage is
  file-local, **any file that imports the `mathffi` wrappers must also declare its own
  `…/clib/libmathlib.a` block** so the C symbols resolve at link time. `bounce.lin`, `ai_worker.lin`
  and `mathffi.test.lin`/`agent.test.lin` each do exactly that — that declaration links the archive
  for the whole binary, satisfying both their own calls and the ones inside the imported wrappers.

What *can* be shared is plain data: the SDL constants and struct byte-offsets live in
`constants.lin` (`export val`s) and both demos `import` them.

## Proof of rendering: `SDL_RenderReadPixels`

After the loop, each demo copies the framebuffer back with `SDL_RenderReadPixels(renderer, NULL)`,
which returns a new `SDL_Surface*` (a `Ptr`), peeks the surface fields and a pixel, and asserts the
colour:

- Surface field byte-offsets are read from the real header and verified with `offsetof` (sizeof 48):
  `format` @ 4, `w` @ 8, `h` @ 12, `pitch` @ 16, `pixels` @ 24. They live in `constants.lin`.
- The software renderer returns surfaces in **`SDL_PIXELFORMAT_XRGB8888`**. On little-endian, each
  4-byte pixel is laid out as **B, G, R, X** at offsets +0, +1, +2, +3. So a channel is read with
  `peekU8(pixels, y*pitch + x*4 + chanOff)` using `PIXEL_B_OFF=0`, `PIXEL_G_OFF=1`, `PIXEL_R_OFF=2`.
- `bounce.lin` clears to `(10,20,30)`, fills the ball with `(255,128,0)`, reads the pixel at the
  centre of the ball's final rect, and asserts `r,g,b == 255,128,0`.
- `ai_worker.lin` fills the agent with `(0,200,120)` and asserts the agent's final-position pixel
  reads back `0,200,120`.

This was confirmed by a C probe against the vendored `.so`: a `(255,128,0)` fill read back bytes
`0,128,255,0`; a `(10,20,30)` clear read back `30,20,10,0`.

## Why the SDL handles never cross the worker boundary

SDL3 has a **main-thread rule**: the window, the event pump (`SDL_PollEvent`), and all rendering
must happen on the thread that called `SDL_Init`. That is *why there is no render thread here* — all
SDL calls stay on the main thread.

Lin's `async` is **share-nothing**: a thunk's captured `val`s are **deep-copied** into the worker
and its result is **deep-copied** back (ADR-028 §2.3). So the division of labour is exactly what SDL
wants: workers traffic in **values** (a `World` snapshot in, a planned `Vec2` out), while
**handles stay on the main thread**. In `ai_worker.lin` the `async` thunk captures only a `val
world` of plain data and returns a plain record — no `Ptr` handle and no `var` ever crosses the
boundary. The checker would reject a thunk that captured a `var` or returned a `Function`/`Iterator`.

> **Honest caveat:** `ai_worker.lin` spawns one `async` thunk **per frame** for clarity. Thread
> spawn per frame is real overhead; a production loop would keep a persistent `worker`/`threadPool`
> (see `std/async`) and only marshal the snapshot. The frame count here is small, so the cost is
> negligible and the by-value transfer is easy to see.

## Run / test

```sh
# Headless (no display) — the dummy driver + software renderer:
SDL_VIDEODRIVER=dummy lin run examples/sdl/bounce.lin
SDL_VIDEODRIVER=dummy lin run examples/sdl/ai_worker.lin

# With a display (real graphics): just omit the variable (or set it to x11/wayland).
lin run examples/sdl/bounce.lin

# Unit tests for the pure modules (vector, matrix, physics, agent, mathffi):
lin test examples/sdl/
```

Expected headless `bounce.lin` output (the ball curves, so its final position differs from a
straight-line bounce):

```
window handle non-null: true
renderer handle non-null: true
frames drawn: 60
final ball: 74.15060115032264,189.604366758501
distance to centre (via C magnitude2): 110.52
pixel[78,193] = 255,128,0
rendered pixel matches fill: true
done
```

Expected headless `ai_worker.lin` output:

```
window handle non-null: true
frames drawn: 60
final agent: 18,11
distance to goal (via C magnitude2): 0.00
pixel[148,92] = 0,200,120
rendered pixel matches fill: true
done
```

FFI works only through `lin build`/`lin run`: the compiler emits LLVM `declare`s, links the `.so`
and the `.a`, and bakes a `$ORIGIN`-relative **rpath** so the produced binary finds the vendored
`.so` at runtime with no `LD_LIBRARY_PATH` (`NEEDED` is the soname `libSDL3.so.0`, resolved through
the symlink chain). The integration tests prove this by running from a different directory with
`LD_LIBRARY_PATH` cleared and `SDL_VIDEODRIVER=dummy` set.

## Don't have SDL3?

The real `.so` is committed, so you don't need a system SDL3 to run these. To rebuild from source:

```sh
# SDL 3.4.10
git clone --branch release-3.4.10 https://github.com/libsdl-org/SDL
cmake -S SDL -B SDL/build -DCMAKE_BUILD_TYPE=Release && cmake --build SDL/build
cp SDL/build/libSDL3.so.0.4.10 examples/sdl/libs/
# recreate the soname symlink chain:
( cd examples/sdl/libs && ln -sf libSDL3.so.0.4.10 libSDL3.so.0 && ln -sf libSDL3.so.0 libSDL3.so )
```

## Limitations (prototype)

- The **dummy video driver** rasterises in software with no GPU and no real display — it proves the
  renderer ran (the pixel readback is exact), but it is not the same as on-screen GPU output.
- The pixel readback **decodes only the XRGB8888** layout the software renderer happens to return.
- `Ptr` is an `Int64` alias, not a distinct opaque type — the checker can't yet forbid arithmetic on
  raw handles.
- The rpath mechanism works on **macOS too** (`@loader_path` token + a best-effort
  `install_name_tool -change` to `@rpath/<leaf>`); the committed `libSDL3.so` is a Linux x86-64
  build, so swap in a macOS SDL3 dylib to run these demos there.
- Committing a ~2.9 MB binary blob is a deliberate convenience for reproducible headless tests.
- `ai_worker.lin` spawns a worker per frame (see the caveat above).

See `crates/lin-runtime/lin.h` for the C/C++ interop header used by the FFI boundary.
