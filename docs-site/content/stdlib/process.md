# std/process

std/process — running and managing external processes, plus OS/system introspection.

Two ways to run a command: `exec`/`shell` run to completion and hand back an ExecResult (exit
status + captured stdout/stderr); `spawn` starts a process without waiting and returns an opaque
ProcessHandle you drive with readStdout / wait / kill. Prefer `exec` over `shell` to avoid shell
injection. Fallible calls return an Error shape you narrow with `is Error`. The system layer
(hostname / platform / arch / cpuCount / pid / username / homeDir / memInfo …, folded in from the
former std/os) reports machine and process facts; the compile-time ones (platform / arch) and the
always-available ones (cpuCount / pid) are total, the rest can return an Error.

import { exec, shell, cwd, chdir, spawn, wait, kill } from "std/process"

## Reference

#### `ExecResult`

```lin
type ExecResult = { "status": Int32, "stdout": String, "stderr": String }
```

The captured result of a finished process: its exit status and full output.

#### `ProcessHandle`

```lin
type ProcessHandle = Int64
```

An opaque handle to a spawned process (an Int64 id, not an OS pid). Used with
readStdout / kill / wait.

### Batch: run to completion, collect all output

#### `exec`

```lin
val exec = (command: String, args: String[]): Json
```

Run `command` with `args` and wait for it to exit, capturing its output.
- **`command`** — the program to run (looked up on PATH; not passed through a shell).
- **`args`** — the argument vector (each element is a separate argument, no shell splitting).
- **Returns** an ExecResult with the exit status and captured stdout/stderr, or an Error if the
  process cannot be launched.
- **Example:** val r = exec("git", ["status", "--short"])   // then r["status"], r["stdout"]

#### `shell`

```lin
val shell = (command: String): Json
```

Run `command` through the system shell (`/bin/sh -c`). Prefer `exec` to avoid shell injection.
- **`command`** — the shell command line.
- **Returns** an ExecResult with the exit status and captured stdout/stderr, or an Error if it
  cannot be launched.
- **Example:** shell("ls -la | wc -l")["stdout"].trim()

#### `cwd`

```lin
val cwd = (): String
```

The absolute path of the current working directory.
- **Example:** cwd()   // "/home/alice/project"

#### `chdir`

```lin
val chdir = (path: String): Json
```

Change the current working directory.
- **`path`** — the directory to switch to.
- **Returns** Null on success, or an Error (e.g. if `path` does not exist or is not a directory).

### Streaming: spawn and read incrementally

#### `spawn`

```lin
val spawn = (command: String, args: String[]): Json
```

Start `command` with `args` without waiting for it to finish. stdout is piped (read it with
readStdout); stderr is inherited.
- **`command`** — the program to run (looked up on PATH).
- **`args`** — the argument vector.
- **Returns** an opaque ProcessHandle, or an Error if the process cannot be launched.

#### `readStdout`

```lin
val readStdout = (handle: ProcessHandle, buf: UInt8[]): Json
```

Read the next chunk of the process's stdout into a caller-provided buffer.
- **`handle`** — the spawned process's handle.
- **`buf`** — the buffer to fill; up to `buf.length` bytes are read.
- **Returns** the number of bytes read (0 = EOF), or an Error.
- **Example:** val n = readStdout(h, buf)   // n bytes copied into buf; 0 means end-of-stream

#### `kill`

```lin
val kill = (handle: ProcessHandle): Json
```

Send SIGTERM to a spawned process.
- **`handle`** — the process to signal.
- **Returns** Null on success, or an Error.

#### `wait`

```lin
val wait = (handle: ProcessHandle): Json
```

Wait for a spawned process to exit. stdout that was streamed via readStdout is not re-collected
here — use `exec` for batch output.
- **`handle`** — the process to wait on.
- **Returns** the process's exit code, or an Error. After `wait` the handle is no longer valid.
- **Example:** val proc = spawn("server", ["--port", "8080"])   // ...later... val code = wait(proc)

#### `stdoutStream`

```lin
val stdoutStream = (handle: ProcessHandle): Stream
```

Wrap a spawned child's piped stdout as a lazy byte Stream (streams brief §4). Reading pulls from
the pipe until EOF.
- **`handle`** — the spawned process's handle.
- **Returns** a `Stream<UInt8[]>` over the child's stdout.

### OS / system introspection (folded in from the former std/os module)

#### `MemInfo`

```lin
type MemInfo = { "total": Int64, "free": Int64 }
```

A snapshot of physical memory, in bytes. `free` is the OS's notion of *available* memory
and its exact meaning varies across platforms (a rough capacity gauge, not exact accounting).

#### `hostname`

```lin
val hostname = (): String | Error
```

The network hostname of the machine.
- **Returns** the short name (e.g. "build-box-3", not an FQDN), or an Error on the rare platform
  where the name cannot be read.

#### `platform`

```lin
val platform = (): String
```

The operating-system family. Fixed at compile time (the build target) — total, never fails.
- **Returns** a lowercase string from a stable closed set: "linux" | "macos" | "windows" |
  "freebsd" | "openbsd" | "netbsd" | "unknown".

#### `arch`

```lin
val arch = (): String
```

The CPU architecture the binary targets. Total.
- **Returns** a lowercase string from a closed set: "x86_64" | "aarch64" | "arm" | "x86" |
  "riscv64" | "wasm32" | "unknown".

#### `cpuCount`

```lin
val cpuCount = (): Int32
```

The number of logical CPUs available to the process (cores x SMT, honouring CPU-affinity /
cgroup limits where exposed). Returns a bare Int32 (not Int32 | Error) so it drops straight
into `threadPool(cpuCount())` without a match.
- **Returns** the CPU count, always >= 1.

#### `pid`

```lin
val pid = (): Int32
```

The OS process id of the current process. Total. Distinct from a ProcessHandle, which is an
opaque id for a child the runtime spawned, not an OS pid.
- **Returns** the current process's pid.

#### `ppid`

```lin
val ppid = (): Int32
```

The OS process id of the parent process (Unix: getppid()).
- **Returns** the parent pid, or 0 where it cannot be determined (rather than an Error), so callers
  can treat "no parent" uniformly.

#### `username`

```lin
val username = (): String | Error
```

The login name of the user the process runs as ("alice", not a display name). Resolved from the
OS (Unix: the effective uid's passwd entry, with $USER / $LOGNAME as a fallback).
- **Returns** the user name, or an Error if no user can be resolved.

#### `homeDir`

```lin
val homeDir = (): String | Error
```

The current user's home directory as an absolute path (Unix: $HOME, falling back to the passwd
entry). For reading $HOME directly use std/env getEnv; homeDir adds the OS-level fallback when
the variable is unset.
- **Returns** the home directory path, or an Error when no home directory is defined for the process.

#### `tempDir`

```lin
val tempDir = (): String
```

The directory the OS designates for temporary files (Unix: $TMPDIR if set, else /tmp). Always
returns *a* path (falls back rather than failing) — total.
- **Returns** the system temp directory as an absolute path.

#### `uptime`

```lin
val uptime = (): Int64 | Error
```

How long the *system* has been running since boot (not process uptime — wall-clock time for the
current program belongs to std/time).
- **Returns** whole seconds since boot, or an Error on a platform where boot time is not queryable.

#### `loadAverage`

```lin
val loadAverage = (): Float64[] | Error
```

The system load average over the last 1, 5, and 15 minutes. Unix-only.
- **Returns** a 3-element Float64[] of load averages, or an Error on platforms without a
  load-average concept (e.g. Windows) — treat the Error as "not available", not a transient
  failure.

#### `memInfo`

```lin
val memInfo = (): MemInfo | Error
```

A snapshot of physical memory, in bytes.
- **Returns** a MemInfo with `total` (installed RAM) and `free` (memory the OS reports as
  available), or an Error if the platform's memory facility cannot be read. Note: `free`
  semantics vary across operating systems — a rough capacity gauge, not exact.
