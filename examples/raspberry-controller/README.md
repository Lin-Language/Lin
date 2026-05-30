# Raspberry-Pi RC car — `deathbot` ported to Lin

A Lin port of [`deathbot`](../../target/tmp/deathbot), a Raspberry-Pi RC car with
two components that talk over UDP on a local WiFi network:

- **Server** (on the Pi): receives 8-byte UDP motor-control packets, drives two
  motors via PWM, and streams the CSI camera as H.264 over RTP.
- **Client** (dev machine): reads the keyboard and sends control packets at 20 Hz.

The original is Rust (`rppal` GPIO, `rpicam-vid` camera, `libc::poll`). Lin has no
GPIO, no subprocess camera capture, and no async runtime, so this port reproduces
the **pure byte/protocol logic** of each component as importable, unit-tested
modules, and **stubs the hardware/OS edges** (clearly marked). The byte-level
protocol code — the actual point of interest — is ported faithfully.

## Modules

| File | Ports | What it is |
| --- | --- | --- |
| `protocol.lin` | `server/src/main.rs::parse_packet`, `client/src/main.rs::encode` | The 8-byte control packet: two big-endian f32 motor speeds, clamped to [-1, 1]. |
| `motor.lin` | `server/src/motor.rs::Motor::set` | The pure speed→PWM mapping (channel + duty). GPIO is **stubbed**. |
| `nal.lin` | `server/src/nal.rs` | H.264 Annex B NAL-unit parser (start-code scanning, nal_type). |
| `rtp.lin` | `server/src/rtp.rs` | RTP packetizer (RFC 6184): header, Single-NAL mode, FU-A fragmentation. |
| `controller.lin` | `client/src/main.rs` | The **client**: keyboard → control packet (the original example, kept). |

Each library module `export`s its functions and has **no top-level side effects**,
so it can be imported by its colocated `*.test.lin`. The tests port the Rust
`#[cfg(test)]` cases as `expect(...).toBe(...)` assertions.

## Protocols

**Control** (client → server, UDP port 3000): 8 bytes — two big-endian IEEE-754
`f32` values (left, right motor speed in `[-1.0, 1.0]`), sent at 20 Hz as a
heartbeat. `protocol.encodePacket` / `protocol.parsePacket`.

**Video** (server → client, RTP/UDP port 3001): H.264 NAL units in RTP packets
(payload type 96, 90 kHz clock). The camera's Annex B byte stream is split into
NAL units (`nal.parseNals`) and each is packetized into RTP — Single-NAL mode for
small NALs, FU-A fragmentation at 1200 bytes for large ones (`rtp.packetize`).

## What is faithfully ported vs stubbed

**Faithfully ported (pure logic):**

- The control packet codec (forward / reverse / stop / turn / clamp-out-of-range,
  and the client encode round-trip).
- The motor speed→PWM mapping (`speed>0` → RPWM, `speed<0` → LPWM, `0` → stop;
  `duty = round(|speed| * PWM_PERIOD)`, `PWM_PERIOD = 1000µs`).
- The NAL start-code scanner (3- and 4-byte start codes; `nal_type = data[0] & 0x1F`).
- The RTP header layout, Single-NAL mode, and FU-A fragmentation bit logic
  (FU indicator `(nal[0] & 0x60) | 28`, FU header with start/end bits).

**Stubbed / omitted (hardware & OS edges):**

- **GPIO/PWM** (`rppal`): `motor.lin` returns a descriptive
  `{ "channel", "duty" }` command instead of toggling pins. A real driver would
  feed this to an FFI GPIO/PWM call (e.g. an `import foreign` binding to a
  `libgpiod`/`pigpio` symbol). No pin I/O is performed.
- **Camera capture** (`rpicam-vid` subprocess + pipe): omitted. The camera itself
  is not portable; the interesting part (NAL + RTP byte processing) is ported and
  would be fed from whatever produces the H.264 stream.
- **The blocking UDP server loop + 500 ms watchdog**: not reproduced as a running
  loop here. `std/net` UDP sockets exist (and `controller.lin`'s `runController`
  shows the live client loop), but the testable core is the pure `parsePacket` /
  `encodePacket` / motor / NAL / RTP logic, which is what these modules expose.

## Simplifications you should know about

- **NAL parser is whole-buffer, not stateful.** The Rust `NalParser` buffers bytes
  across `push()` calls (an `in_nal` flag carrying a partial NAL forward).
  `nal.lin` is a pure whole-buffer parser: `parseNals(buf)` returns the complete
  NAL units in one buffer, with the same flush semantics (a NAL is emitted only
  when a following start code delimits it; trailing bytes after the last start code
  are not emitted). The two **cross-call** Rust tests (`chunked_input`,
  `start_code_split_across_chunks`) are **not ported** — they assert carry-over
  state that a whole-buffer parser does not have. All single-buffer tests are ported.
- **RTP state is threaded functionally.** The Rust `RtpPacketizer` is a `&mut self`
  struct whose `sequence` increments per packet. Lin has no mutable struct (and
  reading `UInt16`/`UInt32` fields back out of a boxed object does not round-trip in
  codegen), so state is passed as explicit typed scalars: `packetize(seq, ts, ssrc,
  nal, marker)` returns the packets; `nextSequence(seq, nal)` and
  `advanceTimestamp(ts)` return the advanced values for the caller to thread into
  the next call.
- **Building large byte buffers in tests.** The FU-A test needs a NAL > 1200 bytes.
  A flat `UInt8[]` built by repeatedly **slicing** corrupts its element reads in the
  current codegen, so the big NAL is grown by `concat` only (see `doubleUp` in
  `rtp.test.lin`). `packetize`'s own internal fragment slicing is correct on a clean
  buffer.
- **`startCodeAt` is tested indirectly.** A standalone unit test that passes a flat
  `UInt8[]` literal local directly into `startCodeAt` trips a pre-existing codegen
  RC double-free (heap-use-after-free in `lin_array_release`, visible under
  `LIN_NO_OPT` + AddressSanitizer; the O2 default build masks it). Start-code
  detection is therefore exercised through `parseNals` (3-byte, 4-byte, mixed,
  no-start-code, empty), which is memory-clean under ASan.

## Run it

```sh
# Run every module's unit tests (this is the primary deliverable)
lin test examples/raspberry-controller/

# Build the client demo (controller.lin has a non-interactive demo())
lin build examples/raspberry-controller/controller.lin -o controller && ./controller
```

To drive a real car, call `controller.runController("<pi-ip>", 3000)` instead of
its `demo()`.

## Which stdlib each module uses

| Module | stdlib |
| --- | --- |
| `protocol.lin` | `std/bytes` (`f32ToBe`/`f32FromBe`), `std/number` (`toFloat32`), `std/math` (`clamp`), `std/array` (`concat`) |
| `motor.lin` | `std/math` (`clamp`/`round`/`abs`), `std/number` (`toInt32`) |
| `nal.lin` | `std/array` (`slice`/`length`/`push`) |
| `rtp.lin` | `std/bytes` (`u16ToBe`/`u32ToBe`), `std/array` (`concat`/`slice`/`length`/`push`), `std/number` (`toUInt8`) |
| `controller.lin` | `std/bytes`, `std/number`, `std/math`, `std/net`, `std/tty`, `std/time`, `std/array`, `std/io`, `std/string` |
