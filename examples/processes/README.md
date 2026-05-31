# Processes

Run external processes with `std/process`, two ways: `exec` (batch — run to
completion and collect the full output) and `spawn`/`readStdout`/`wait` (streaming
— read piped stdout incrementally). A small tour over real child processes.

## What it demonstrates

- `std/process` batch API: `exec(command, args)` → `ExecResult { status, stdout, stderr }`.
- `std/process` streaming API: `spawn(command, args)`, `readStdout(handle, buf)`, `wait(handle)`.
- Flat `UInt8[]` byte buffers as read targets (`readStdout` fills the buffer in place).
- Reading a child's bytes and propagating its exit code.

## Structure

- **`main.lin`** — `exec("printf", ["Hello"])` for batch output, then spawns
  `sh -c "printf 'Hi'"`, reads its stdout, and prints the bytes and exit code.
- **`process.test.lin`** — asserts the read byte count, the actual bytes (`'H'`=72, `'i'`=105),
  and a non-zero exit code from `sh -c "exit 3"`.

## Typing note

`ExecResult` is a named record type exported by `std/process`. The process handle
from `spawn` is an opaque `ProcessHandle` (an `Int64` id, not an OS pid). The stdout
buffer is a flat `UInt8[]` and the argument list is `String[]`. (Lower-level
spawn/exit-code assertions also live in the Rust integration suite.)

## Run / Test

```bash
lin run examples/processes/main.lin      # exec batch output, then spawn + read bytes
lin test examples/processes/             # the recoverable, deterministic assertions
```
