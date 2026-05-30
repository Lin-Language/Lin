# Raspberry-Pi RC-car controller

A Lin port of the keyboard control client from the `deathbot` project (a
Raspberry-Pi RC car). It is the **capstone** for Milestone 21 (low-level
primitives): a single small program that exercises nearly the whole low-level
stdlib added in that milestone.

## What it does

The original Rust client puts the terminal in raw mode, reads arrow-ish keys, and
sends an **8-byte UDP control packet at 20 Hz** — two big-endian IEEE-754 `f32`
motor speeds in `[-1.0, 1.0]`:

```
q / a  →  left  +0.1 / -0.1        w / s  →  right +0.1 / -0.1
space  →  stop (both 0)            ESC    →  zero speeds and quit
```

`controller.lin` reproduces the protocol and control logic faithfully.

## Which stdlib it exercises

| Module | Used for |
| --- | --- |
| `std/bytes` | `f32ToBe` — big-endian f32 serialization of each motor speed |
| `std/number` | `toFloat32` — narrow the computed `Float64` speed to `Float32` |
| `std/net` | `udpBind` / `udpSendTo` — send the control packet |
| `std/tty` | `rawMode` / `readKey` — raw-mode, non-blocking keyboard |
| `std/math` | `clamp` / `round` — quantise + clamp speeds |
| `std/time` | `sleep` — the 20 Hz tick |
| `std/array` | `push` / `range` / `for` — assemble the byte buffer, loop |

## Structure

- **`clampSpeed`, `encodePacket`, `applyKey`** — pure functions (the protocol +
  control core). Fully unit-tested in `controller.test.lin`.
- **`runController`** — the real interactive loop (TTY + UDP). Not run by CI (it
  needs a live terminal and a listening peer), but it is the faithful client.
- **`demo`** — a non-interactive smoke run so the example produces output when run
  directly.

## Run it

```sh
lin build examples/raspberry-controller/controller.lin -o controller && ./controller
lin test examples/raspberry-controller/      # the unit tests
```

To drive a real car, call `runController("<pi-ip>", 3000)` instead of `demo()`.

## Notes

- The 8-byte packet is assembled with `push` rather than `std/array.concat`,
  because `concat` does not yet preserve a flat `UInt8[]`'s element width (see
  `docs/TODO.md`).
- The step size is written inline (`0.1`) rather than as a module-level `val`,
  because a top-level non-function `val` referenced inside an *imported* function
  currently mis-lowers (see `docs/TODO.md`).
