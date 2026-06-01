# COMPRESS_PROGRESS — branch `feat/compress`

Two deliverables. **Not merged to master** — left on `feat/compress` for review.

## A. `std/compress` — streaming gzip/deflate byte-adapters (mergeable increment)

Four lazy `Stream<UInt8[]> -> Stream<UInt8[]>` adapters that (de)compress bytes incrementally,
reusing the existing stream-adapter machinery:

- `gunzip(s)` / `gzip(s)` — gzip container format (header + CRC32 + length trailer)
- `inflate(s)` / `deflate(s)` — raw DEFLATE (no gzip header/CRC)

### Runtime (`crates/lin-runtime/src/stream.rs`)
- One `CodecSource: StreamSource` adapter following the exact RC discipline of `ChunksSource`/
  `LinesSource`: pull one upstream chunk per `read_tagged`, release each pulled item after
  consuming its bytes, return a freshly-owned `UInt8[]` box, propagate `TaggedOutcome::Err`
  straight through, finish+flush the codec at upstream EOF, close the upstream in `close()`.
- Four `#[no_mangle] extern "C"` entry points mirroring `lin_stream_chunks`' ownership shape
  (`own_upstream(s)` then box a new source): `lin_stream_gunzip/gzip/inflate/deflate`.
- New Rust unit tests in the `#[cfg(test)] mod tests` block, all asserting close-once via the
  `close_count` pattern: `deflate_then_inflate_round_trips_and_closes_once`,
  `gzip_then_gunzip_round_trips_and_closes_once`,
  `gunzip_of_garbage_errors_in_band_and_closes_once`.

### Wiring
- `crates/lin-ir/src/ir.rs`: four new `Intrinsic` variants (`StreamGunzip/Gzip/Inflate/Deflate`).
- `crates/lin-ir/src/lower.rs`: name → intrinsic mapping for the four `lin_stream_*` symbols.
- `crates/lin-codegen/src/codegen/intrinsics.rs`: a single-stream-arg dispatch arm (modelled on
  `StreamFlatten`) that declares + calls each runtime fn.
- `crates/lin-check/src/checker/intrinsics.rs`: each typed `Stream -> Stream`.
- `stdlib/compress.lin`: exports `gunzip`/`gzip`/`inflate`/`deflate`, each calling its intrinsic.
- `crates/lin-compile/src/lib.rs`: registered `std/compress` in the embedded-stdlib loader.
- `stdlib/compress.test.lin`: disk round-trips (gzip→gunzip, deflate→inflate via `writeFileBytes`),
  gzip-magic check, compression-shrinks check, and an in-band-Error-on-garbage check.
- `docs/STDLIB.md` + `CLAUDE.md`: documented the four fns and added `std/compress` to the module list.
- `crates/lin-runtime/Cargo.toml`: added `flate2 = "1"` (already in the lockfile via `ureq`;
  pure-Rust `miniz_oxide` backend, no libz C dep).

### Deviation
The brief asked for the low-level `flate2::Compress`/`Decompress` mem API. The gzip CONTAINER
framing via that API (`Compress::new_gzip`/`Decompress::new_gzip`) is gated behind a C-zlib feature
we deliberately do NOT enable (we keep the pure-Rust miniz_oxide backend). All four codecs instead
use flate2's WRITE-based streaming wrappers (`GzEncoder`/`MultiGzDecoder`/`DeflateEncoder`/
`DeflateDecoder`), each writing into an owned `Vec<u8>` sink we drain after every fed chunk. This is
equally incremental (one upstream chunk in, drained output out — no whole-buffer convenience fns)
and gives one uniform driver for all four codecs. Behaviour (lazy, constant-memory, in-band errors)
matches the brief.

## B. `untar` sub-stream FEASIBILITY SPIKE (runtime-only, no language surface)

Prototyped the shared-cursor sub-stream mechanism entirely behind `#[cfg(test)]` in
`crates/lin-runtime/src/stream.rs` — NO stdlib export, NO codegen, NO new soundness machinery:

- `Arc<Mutex<TarReaderState>>` holds the parent `Upstream`, a byte pushback buffer, an
  `upstream_done` flag, and `current_entry_remaining`.
- `parse_tar_header`: 512-byte block; name (offset 0, 100 bytes, NUL-trimmed), octal-ASCII size
  (offset 124, 12 bytes), typeflag (offset 156); all-zero block = end-of-archive.
- `BoundedSource: StreamSource` holds a clone of the Arc and yields at most
  `current_entry_remaining` bytes from the shared buffer, then EOF (the per-entry `data` sub-stream);
  its `close()` is a no-op (it must NOT close the shared parent).
- Driver in the test: parse header → set `current_entry_remaining` → (optionally) drain the
  `BoundedSource` → read back the undrained remainder and skip it + 512-padding to the next header.

### Test: `untar_shared_cursor_spike_drains_skips_and_closes_once`
Builds an in-memory tar with a ~200 KiB entry then a small entry, fed through a `CountingSource`,
and asserts:
1. draining the large entry's `BoundedSource` yields exactly its bytes then EOF;
2. a NON-drained entry is correctly skipped (skip undrained body + padding) so the next header
   parse lands on the end-of-archive block;
3. the parent upstream closes exactly once.

### Feasibility verdict: FEASIBLE.
The shared-cursor `Arc<Mutex<TarReaderState>>` + `BoundedSource` cleanly handles the
parse-header / hand-off-body / skip-undrained-body handoff with NO new soundness machinery — it
reuses the existing `Upstream`/`pull_tagged`/`bytes_to_u8_array` primitives and the established
close-once discipline. The only real subtlety (correctly skipping an un-drained entry body so the
next header parses) is proven by the test. A future `untar(s)` language surface would wrap this in
an adapter that emits `(meta, dataStream)` pairs; the runtime mechanism is ready.

## Test results
- `cargo test -p lin-runtime` — 64 passed, 0 failed (includes 3 compress tests + the untar spike).
- Same 24 stream tests pass clean under AddressSanitizer (`-Zsanitizer=address`, nightly) — no
  UAF/double-free in the new close-propagation paths.
- `cargo run -p lin -- test stdlib/` — 23 test files passed, including `stdlib/compress.test.lin`.
- `cargo test --workspace`: all crates pass; the single `lin` integration crate shows ONE failure,
  `test_http_fetch_json`, ONLY under parallel execution. It passes in isolation and passes the full
  integration suite single-threaded (`--test-threads=1`: 415 passed, 0 failed). It is a pre-existing
  flaky port/timing race in the in-process tiny_http server test, unrelated to this branch (no HTTP
  code was touched).

## NOT merged
Left on branch `feat/compress`. Awaiting review before any merge to master.
