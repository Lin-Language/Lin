# mocking — test mocking with `replace` + dependency injection

An audit logger built from two side-effecting dependencies — a **clock** (wall time)
and a **store** (filesystem) — and the tests that isolate it from both using Lin's
test-only `replace` mock (ADR-071). `main.lin` runs the real thing; the `*.test.lin`
suites run the same code paths with time pinned and the filesystem replaced by an
in-memory spy.

## What it demonstrates

- **`replace <name> = <expr>`** — the test-only symbol override. After
  `import { nowMs } from "./clock"`, a `replace nowMs = () => 1609459200000` swaps the
  definition for the *whole test program*.
- **The Option-A "replaced everywhere" guarantee** — `clock.test.lin` replaces only
  `nowMs`, yet the sibling `stamp()` (which calls `nowMs` internally) also sees the
  mock. A `replace` overrides the export's symbol for every caller — the unit under
  test, transitive importers, however the path is spelled.
- **Mocking the stdlib** — `store.test.lin` replaces `std/fs`'s `appendFile`/`readFile`
  wrappers, so the test never touches a real file. Stdlib wrappers are ordinary
  compiled Lin and are mockable at the Lin-API level.
- **Spies with no extra framework** — a mock closes over a module-level `var`/`val`
  cell to record the arguments it was called with (`lastLine`, `appendCalls`, the
  accumulated `lines` array), asserted after the run.
- **Dependency injection by composition** — `logger.lin` is written against its local
  imports and is oblivious to mocking; the tests substitute its dependencies at link
  time rather than threading fakes through its signatures.
- **Type-checked mocks** — a `replace` body is checked against the export's real
  signature, so a drifting mock (`replace nowMs = (): String => ...`) is a compile
  error, not a silent mismatch.

## `replace` is test-only

`replace` is permitted only in a `*.test.lin` file. Using it in a program compiled
with `lin build`/`lin run` is a hard compile error — a shipped binary must never
silently swap a real import (e.g. stdlib `fs`).

## Structure

| File | What it is |
| --- | --- |
| `clock.lin` | `nowMs`/`stamp` over `std/time`. |
| `store.lin` | `append`/`readAll` over `std/fs`. |
| `logger.lin` | `formatLine`/`log`/`info`/`error` composed from the clock + store. |
| `main.lin` | Logs three audit lines with the REAL clock + store, then reads them back. |
| `clock.test.lin` | Mocks `nowMs`; asserts `stamp` sees it (the everywhere guarantee). |
| `store.test.lin` | Mocks the `std/fs` wrappers; spies on the forwarded arguments. |
| `logger.test.lin` | Mocks both `stamp` and `append`; asserts the formatted line. |
| `integration.test.lin` | End-to-end: a mocked clock + in-memory store, asserting the full transcript. |

## Run it

```bash
cargo run -p lin -- run examples/mocking/main.lin     # real clock + filesystem
cargo run -p lin -- test examples/mocking/            # all four suites, fully mocked
```
