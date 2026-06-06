# sdl — an SDL3 "game" in pure Lin (richer FFI + share-nothing concurrency)

Two small demos that drive **real SDL3 3.4.10** — `SDL_Init`, `SDL_CreateWindow`,
`SDL_CreateRenderer`, `SDL_SetRenderDrawColor`, `SDL_RenderClear`, `SDL_RenderFillRect`,
`SDL_RenderPresent`, `SDL_RenderReadPixels`, `SDL_DestroySurface`, `SDL_PollEvent`, `SDL_Delay`,
`SDL_DestroyWindow`, `SDL_Quit` — entirely from pure Lin via `import foreign` and the `std/ffi`
raw-memory helpers:

- **`bounce.lin`** — a bouncing ball. Opens a window (a `char*` title marshalled with `withCstr`,
  a window handle carried as a `Ptr`), creates a renderer, then runs a tail-recursive frame loop
  over a **fixed frame count**: drain events, clear, build an `SDL_FRect` in a 16-byte buffer with
  four `pokeF32`, set the colour, fill, present. Ball physics live in an immutable `{x, y, vx, vy}`
  record threaded through the loop.
- **`ai_worker.lin`** — the same SDL main-thread loop **plus** an `async` worker. Each frame a
  plain `World` snapshot is deep-copied into an `async` thunk that computes the agent's next step
  toward a goal (a pure function), the result `{x, y}` is deep-copied back, and the agent is drawn
  on the main thread.

## Real SDL3 — vendored, headless via the dummy driver

`libs/` contains the **real** `libSDL3.so.0.4.10` (~2.9 MB) plus the soname symlink chain
`libSDL3.so -> libSDL3.so.0 -> libSDL3.so.0.4.10`. All three are committed: git tracks symlinks,
the library's soname is `libSDL3.so.0` (so the produced binary's `NEEDED` entry is `libSDL3.so.0`),
and the `$ORIGIN`-rpath loader resolves that soname through the chain at runtime. The demos import
it by relative path: `import foreign "examples/sdl/libs/libSDL3.so"`.

There is **no display in CI**, so the demos run **headless**: set `SDL_VIDEODRIVER=dummy` in the
environment and SDL3 selects the dummy video driver plus the **software renderer** — real
rasterisation, no GPU or display needed. (On a machine with a display, do *not* set the variable —
or set `SDL_VIDEODRIVER=x11`/`wayland` — to see actual graphics in a window.)

Real headless SDL3 emits **no synthetic `SDL_EVENT_QUIT`**, so a poll-to-quit loop would hang
forever. The demos therefore run a **fixed frame count** (60) and self-terminate. They still
*pump* the event queue each frame (draining and discarding), they just don't depend on it to stop.

## Proof of rendering: `SDL_RenderReadPixels`

After the loop, each demo copies the framebuffer back with `SDL_RenderReadPixels(renderer, NULL)`,
which returns a new `SDL_Surface*` (a `Ptr`). From that pointer it peeks the surface fields and a
pixel and asserts the colour:

- The surface field byte-offsets are read from the real header
  (`/tmp/sdl3-built/include/SDL3/SDL_surface.h`, `struct SDL_Surface`) and verified with `offsetof`
  (sizeof 48): `format` @ 4, `w` @ 8, `h` @ 12, `pitch` @ 16, `pixels` @ 24 (8-byte aligned, 4 bytes
  of padding at 20–23). They live in `constants.lin` as `SURFACE_*_OFF`.
- The software renderer returns surfaces in **`SDL_PIXELFORMAT_XRGB8888`** (= `0x16161804` =
  `370546692`). On little-endian, each 4-byte pixel is laid out in memory as **B, G, R, X** at byte
  offsets +0, +1, +2, +3. So a channel is read with `peekU8(pixels, y*pitch + x*4 + chanOff)` using
  `PIXEL_B_OFF=0`, `PIXEL_G_OFF=1`, `PIXEL_R_OFF=2`.
- `bounce.lin` clears to `(10,20,30)`, fills the ball with `(255,128,0)`, then reads the pixel at
  the centre of the ball's final rect and asserts `r,g,b == 255,128,0`.
- `ai_worker.lin` fills the agent with `(0,200,120)` and asserts the agent's final-position pixel
  reads back `0,200,120`.

This was confirmed by a C probe against the vendored `.so`: a `(255,128,0)` fill read back bytes
`0,128,255,0`; a `(10,20,30)` clear read back `30,20,10,0`.

## Why the SDL handles never cross the worker boundary

SDL3 has a **main-thread rule**: the window, the event pump (`SDL_PollEvent`), and all rendering
must happen on the thread that called `SDL_Init`. You cannot legally render from a background
thread. That is *why there is no render thread here* — all SDL calls stay on the main thread.

Lin's `async` is **share-nothing**: a thunk's captured `val`s are **deep-copied** into the worker
thread and its result is **deep-copied** back (ADR-028 §2.3). So the natural division of labour is
exactly what SDL wants: workers traffic in **values** (a `World` snapshot in, a planned point out),
while **handles stay on the main thread**. In `ai_worker.lin` the `async` thunk captures only a
`val world` of plain data and returns a plain record — no `Ptr` handle and no `var` ever crosses
the boundary. The checker would reject a thunk that captured a `var` or returned a `Function`/
`Iterator`; a raw `Ptr` is just an `Int64`, so passing one in would be a *bug we deliberately
avoid* — it would name a pointer that is meaningless on the other thread.

> **Honest caveat:** `ai_worker.lin` spawns one `async` thunk **per frame** for clarity. Thread
> spawn per frame is real overhead; a production loop would keep a persistent `worker`/`threadPool`
> (see `std/async`) and only marshal the snapshot. The frame count here is small, so the cost is
> negligible and the by-value transfer is easy to see.

## File-local foreign bindings

`import foreign` bindings are **file-local** — the library is linked only for the file that
declares the block, and the bindings **cannot be re-exported** from a wrapper module. So
`bounce.lin` and `ai_worker.lin` each declare their **own** `import foreign "examples/sdl/libs/libSDL3.so"`
block with the real `SDL_*` signatures. What *can* be shared is plain data: the SDL constants and
struct byte-offsets live in **`constants.lin`** (`export val`s) and both demos `import` them.

## Files

- **`libs/libSDL3.so.0.4.10`** — the real SDL3 3.4.10 shared library (~2.9 MB).
- **`libs/libSDL3.so.0`**, **`libs/libSDL3.so`** — the soname symlink chain (committed).
- **`constants.lin`** — shared SDL constants + `SDL_FRect`/`SDL_Event`/`SDL_Surface` byte-offsets
  and the XRGB8888 pixel-channel offsets (`export val`).
- **`bounce.lin`** — the bouncing-ball demo.
- **`ai_worker.lin`** — the SDL + async-pure-worker demo.

(A hand-written headless `sdl3_stub.c` used to live here; it has been removed now that the demos
run against real SDL3.)

## Build / run

```sh
# Headless (no display) — the dummy driver + software renderer:
SDL_VIDEODRIVER=dummy lin run examples/sdl/bounce.lin
SDL_VIDEODRIVER=dummy lin run examples/sdl/ai_worker.lin

# With a display (real graphics): just omit the variable (or set it to x11/wayland).
lin run examples/sdl/bounce.lin
```

Expected headless `bounce.lin` output:

```
window handle non-null: true
renderer handle non-null: true
frames drawn: 60
final ball: 180.0,120.0
pixel[184,124] = 255,128,0
rendered pixel matches fill: true
done
```

Expected headless `ai_worker.lin` output:

```
window handle non-null: true
frames drawn: 60
final agent: 18,11
pixel[148,92] = 0,200,120
rendered pixel matches fill: true
done
```

FFI works only through `lin build`/`lin run`: the compiler emits LLVM `declare`s, links the `.so`,
and bakes a `$ORIGIN`-relative **rpath** so the produced binary finds the vendored `.so` at runtime
with no `LD_LIBRARY_PATH` (`NEEDED` is the soname `libSDL3.so.0`, resolved through the symlink
chain). The integration tests prove this by running from a different directory with
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
- The pixel readback **decodes only the XRGB8888** layout the software renderer happens to return;
  a different driver/format would need its channel order re-derived from `format`.
- `Ptr` is an `Int64` alias, not a distinct opaque type — the checker can't yet forbid arithmetic
  on raw handles or `Int64`↔`Ptr` confusion.
- The rpath mechanism works on **macOS too** (`@loader_path` token + a best-effort
  `install_name_tool -change` to `@rpath/<leaf>`); the committed `libSDL3.so` is a Linux x86-64
  build, so swap in a macOS SDL3 dylib to run these demos there.
- Committing a ~2.9 MB binary blob in the repo is a deliberate convenience for reproducible
  headless tests; a real project would depend on a system/package-managed SDL3.
- `ai_worker.lin` spawns a worker per frame (see the caveat above).
