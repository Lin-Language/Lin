# std/process

Running and managing external processes.

```lin
import { exec, shell, cwd, chdir, spawn, wait, kill } from "std/process"
```

## Types

```lin
type ExecResult = {
  "status": Int32,    // exit code
  "stdout": String,
  "stderr": String
}
```

`ProcessHandle` is an opaque runtime type returned by `spawn`.

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `chdir` | `(String) -> Null \| Error` | Change working directory |
| `cwd` | `() -> String` | Current working directory |
| `exec` | `(String, String[]) -> ExecResult \| Error` | Run command and collect output |
| `kill` | `(ProcessHandle) -> Null \| Error` | Send SIGTERM to spawned process |
| `readStdout` | `(ProcessHandle, UInt8[]) -> Int32 \| Error` | Read piped stdout into a buffer (0 = EOF) |
| `shell` | `(String) -> ExecResult \| Error` | Run shell command string |
| `spawn` | `(String, String[]) -> ProcessHandle \| Error` | Start process without waiting |
| `wait` | `(ProcessHandle) -> Int32 \| Error` | Wait for spawned process; returns exit code |

---

### `exec`

```lin
match exec("git", ["status", "--short"])
  has { "type": "failure", error } => print("exec failed: ${error}")
  else =>
    val r = exec("git", ["status", "--short"])
    print("exit ${r["status"]}")
    print(r["stdout"])
```

---

### `shell`

```lin
match shell("ls -la | wc -l")
  has { "type": "success", value } => print(value["stdout"].trim())
  has { "type": "failure", error } => print("error: ${error}")
```

Prefer `exec` when possible to avoid shell injection vulnerabilities.

---

### `cwd` / `chdir`

```lin
val here = cwd()   // "/home/alice/project"

match chdir("src")
  has { "type": "failure", error } => print("cannot chdir: ${error}")
  else => null
```

---

### `spawn` / `wait` / `kill`

```lin
val proc = spawn("server", ["--port", "8080"])
// ... do other work ...
val exitCode = wait(proc)   // exit code, or -1 if signalled

// Or kill it:
kill(proc)
```

After `wait` the handle is no longer valid.

---

### `readStdout`

`readStdout` reads a spawned process's piped stdout incrementally into a caller-owned `UInt8[]`, returning the number of bytes read (`0` means end-of-stream):

```lin
import { spawn, readStdout, wait } from "std/process"

val h = spawn("sh", ["-c", "printf hello"])
val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
val n = readStdout(h, buf)   // n == 5; buf[0] == 104 ('h')
wait(h)
```
