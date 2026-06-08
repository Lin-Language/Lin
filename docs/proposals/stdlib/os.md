# std/os — design proposal

## Status: proposal

Lin can already read the environment (`std/env`), run subprocesses (`std/process`), and
read the command line and exit (`std/io`), but it has no way to ask the machine about
*itself*: what host am I on, what platform and CPU architecture, how many logical cores,
what is my pid, who is the current user, where is the temp dir. Every mainstream language
exposes this — Node (`os`), Python (`platform` / `os`), Go (`runtime` / `os`), Java
(`System` properties). Today a Lin program has to shell out (`exec("uname", [...])`,
`exec("hostname", [])`) and parse text, which is slow, non-portable, and fragile. This
module fills that system-introspection gap with a small set of direct, portable
intrinsics.

The single most load-bearing call is `cpuCount()`: it is the natural argument to
`std/async`'s `threadPool(n)` so a program can size its worker pool to the host instead of
hard-coding a number. The rest are diagnostic / configuration helpers (logging the host
and pid, locating the temp dir, branching on platform). The module is **read-only
introspection** — it does not mutate anything; environment mutation stays in `std/env`,
working-directory changes stay in `std/process`, and process termination stays in
`std/io` (`exit`).

---

## std/os

Query the host operating system, machine, and current process. Every function in this
module is a thin Rust runtime intrinsic. Most are total (they always return a value);
the handful that depend on OS facilities not present on every platform return the
canonical `T | Error` result shape (spec §27.6) so a program can degrade gracefully
rather than fault.

Import:

```txt
import { hostname, platform, arch, cpuCount } from "std/os"
import { pid, ppid, username, homeDir, tempDir } from "std/os"
import { uptime, loadAverage, memInfo } from "std/os"
```

`exit` is **not** here — terminating the process lives in [`std/io`](#stdio) (`exit(code)`).

### Summary

```txt
hostname:     ()  => String | Error
platform:     ()  => String              // "linux" | "macos" | "windows" | "freebsd" | …
arch:         ()  => String              // "x86_64" | "aarch64" | "arm" | "x86" | …
cpuCount:     ()  => Int32               // logical cores, always >= 1
pid:          ()  => Int32
ppid:         ()  => Int32
username:     ()  => String | Error
homeDir:      ()  => String | Error
tempDir:      ()  => String
uptime:       ()  => Int64 | Error       // whole seconds since boot
loadAverage:  ()  => [Float64, Float64, Float64] | Error   // 1, 5, 15-min; Unix-only
memInfo:      ()  => MemInfo | Error
```

### Types

```txt
type MemInfo = { "total": Int64, "free": Int64 }   // bytes
```

---

### hostname

```txt
val hostname: () -> String | Error
```

The network hostname of the machine (the short name, e.g. `"build-box-3"`, not a fully
qualified domain name). Returns an `Error` on the rare platform where the name cannot be
read.

```txt
val h = hostname()
match h
  is Error => printErr("no hostname: ${h["message"]}")
  else     => print("running on ${h}")
```

---

### platform

```txt
val platform: () -> String
```

The operating-system family the program is running on, as a lowercase string. The values
are a stable, closed set — branch on them with string equality:

| value       | OS                       |
|-------------|--------------------------|
| `"linux"`   | Linux                    |
| `"macos"`   | macOS / Darwin           |
| `"windows"` | Windows                  |
| `"freebsd"` | FreeBSD                  |
| `"openbsd"` | OpenBSD                  |
| `"netbsd"`  | NetBSD                   |
| `"unknown"` | anything else            |

The value is fixed at compile time (it is the target the binary was built for), so this
call is total and never fails.

```txt
val sep = platform() == "windows" ? "\\" : "/"
```

> Note: `"macos"` is used rather than `"darwin"` to match the user-facing OS name (this
> differs from Rust's `std::env::consts::OS`, which reports `"macos"` too, but matches
> Node's `os.platform()` returning `"darwin"` — Lin deliberately normalises to `"macos"`).

---

### arch

```txt
val arch: () -> String
```

The CPU architecture the binary targets, as a lowercase string from a closed set:
`"x86_64"`, `"aarch64"`, `"arm"`, `"x86"`, `"riscv64"`, `"wasm32"`, or `"unknown"`. Like
`platform`, it is fixed at compile time and never fails.

```txt
print("${platform()}/${arch()}")   // e.g. "linux/x86_64"
```

---

### cpuCount

```txt
val cpuCount: () -> Int32
```

The number of **logical** CPUs available to the process (hardware threads, i.e. cores ×
SMT, honouring CPU-affinity / cgroup limits where the platform exposes them). Always
returns at least `1`. This is the natural argument to `std/async`'s `threadPool`:

```txt
import { cpuCount } from "std/os"
import { threadPool } from "std/async"

val pool = threadPool(cpuCount())   // one worker per logical core
```

---

### pid

```txt
val pid: () -> Int32
```

The operating-system process id of the current process. Total; never fails.

```txt
print("pid ${pid()}")
```

> This is the OS pid, distinct from a `std/process` `ProcessHandle` (which is an opaque
> monotonic id for a *child* the runtime spawned, not an OS pid — see `std/process`).

---

### ppid

```txt
val ppid: () -> Int32
```

The OS process id of the **parent** process. On Unix this is `getppid()`. On Windows the
parent pid is not a first-class concept; the runtime resolves it via a process snapshot,
and returns `0` if it cannot be determined (rather than an `Error`, so a program can treat
"no parent" uniformly).

```txt
print("launched by pid ${ppid()}")
```

---

### username

```txt
val username: () -> String | Error
```

The login name of the user the process runs as (`alice`, not a full display name).
Resolved from the OS (Unix: the effective uid's passwd entry, with `$USER` as a fallback;
Windows: `GetUserName`). Returns an `Error` if no user can be resolved (e.g. a uid with no
passwd entry inside a minimal container).

```txt
val u = username()
match u
  is Error => print("unknown user")
  else     => print("hello ${u}")
```

---

### homeDir

```txt
val homeDir: () -> String | Error
```

The current user's home directory as an absolute path (Unix: `$HOME`, falling back to the
passwd entry; Windows: the user profile directory). Returns an `Error` when no home
directory is defined for the process.

```txt
val home = homeDir()   // e.g. "/home/alice"
```

> For reading arbitrary environment variables (including `$HOME` directly) use
> [`std/env`](#stdenv) `getEnv`; `homeDir` adds the OS-level fallback when the variable is
> unset.

---

### tempDir

```txt
val tempDir: () -> String
```

The directory the OS designates for temporary files, as an absolute path — Unix:
`$TMPDIR` if set, else `/tmp`; Windows: `%TEMP%` / `%TMP%` / the Windows temp dir. Always
returns *a* path (it falls back to `/tmp` resp. the system default rather than failing),
so the call is total.

This is intended to pair with the filesystem-extras `tempFile` proposal: `std/os` exposes
only the *directory*; creating a uniquely-named temp file inside it (with the
create-exclusive race-free semantics) belongs to `std/fs`.

```txt
import { tempDir } from "std/os"
import { join } from "std/path"

val scratch = join([tempDir(), "myapp.cache"])   // e.g. "/tmp/myapp.cache"
```

---

### uptime

```txt
val uptime: () -> Int64 | Error
```

How long the **system** has been running, in whole seconds since boot. Returns an `Error`
on a platform where boot time is not queryable. The unit is seconds (not milliseconds):
seconds is the natural granularity for uptime, fits an `Int64` for any plausible duration,
and avoids implying sub-second precision the OS does not provide.

```txt
val up = uptime()
match up
  is Error => print("uptime unavailable")
  else     => print("up ${up / 3600} hours")
```

> This is *system* uptime (since boot), not process uptime. Wall-clock time and durations
> for the current program belong to [`std/time`](#stdtime).

---

### loadAverage

```txt
val loadAverage: () -> [Float64, Float64, Float64] | Error
```

The system load average over the last 1, 5, and 15 minutes (a 3-element tuple-shaped
array). **Unix-only**: on Windows there is no load-average concept and this returns an
`Error`. Treat the `Error` as "not available on this platform", not as a transient
failure.

```txt
val la = loadAverage()
match la
  is Error => print("load average unavailable (non-Unix)")
  else     => print("1-min load: ${la[0]}")
```

---

### memInfo

```txt
val memInfo: () -> MemInfo | Error
```

A snapshot of physical memory, in **bytes**: `total` (installed RAM) and `free` (memory
the OS reports as available). Returns an `Error` if the platform's memory facility cannot
be read.

```txt
val m = memInfo()
match m
  is Error => print("mem info unavailable")
  else     => print("${m["free"]} / ${m["total"]} bytes free")
```

> Platform variance: `free` is the OS's notion of *available* memory and is not directly
> comparable across operating systems (Linux's `MemAvailable` vs macOS's free+inactive
> pages vs Windows' available physical bytes). It is a rough capacity gauge, not an exact
> accounting figure. Per-process resident memory and richer breakdowns (cached, swap,
> buffers) are deliberately out of scope for this proposal.

---

## Implementation notes

### Intrinsics and the backing crate

Each export is a one-line wrapper over a `lin_os_*` foreign intrinsic, exactly like
`stdlib/env.lin` wraps `lin_env_*`:

```txt
import foreign "lin-runtime"
  val lin_os_hostname:    () => Json     // String | Error
  val lin_os_platform:    () => String
  val lin_os_arch:        () => String
  val lin_os_cpu_count:   () => Int32
  val lin_os_pid:         () => Int32
  val lin_os_ppid:        () => Int32
  val lin_os_username:    () => Json     // String | Error
  val lin_os_home_dir:    () => Json     // String | Error
  val lin_os_temp_dir:    () => String
  val lin_os_uptime:      () => Json     // Int64 | Error
  val lin_os_load_average:() => Json     // [Float64;3] | Error
  val lin_os_mem_info:    () => Json     // MemInfo | Error
```

(The `Json`-typed intrinsics are narrowed to their public `T | Error` types in the `.lin`
wrappers, as `std/env`/`std/process` already do; `is Error` discrimination then works at
the call site.)

The Rust side splits into two tiers:

- **Trivially portable — `std` only, no extra crate.** `platform` and `arch` are
  `std::env::consts::OS` / `ARCH` (compile-time constants — `platform` just normalises
  `"darwin"`-style names to Lin's `"macos"`). `cpuCount` is
  `std::thread::available_parallelism()` (already affinity/cgroup-aware, returns a
  `NonZero`, so `>= 1` is guaranteed). `pid` is `std::process::id()`. `tempDir` is
  `std::env::temp_dir()`. `hostname` and `username`/`homeDir` are thin libc/`whoami`-style
  lookups. These need no third-party dependency and behave identically on every supported
  target.

- **Platform-specific — back with the [`sysinfo`](https://docs.rs/sysinfo) crate.**
  `uptime`, `loadAverage`, and `memInfo` (and a robust cross-platform `ppid`) are exactly
  what `sysinfo` exists to provide: `System::load_average()`, `System::uptime()`,
  `System::total_memory()` / `available_memory()`, and the process table for `ppid`.
  Using `sysinfo` avoids hand-rolling `/proc` parsing on Linux, `sysctl`/`host_statistics`
  on macOS, and the Win32 perf APIs on Windows, and gives a uniform `T | Error` surface
  where a metric is genuinely unavailable. The crate is already a candidate dependency for
  any future `std/sys`-style observability; pulling it in for this module is the natural
  first use.

The intrinsics returning a freshly-allocated record (`memInfo`'s `MemInfo`) or array
(`loadAverage`'s 3-element `Float64[]`) must follow the owned-result RC contract — a fresh
`+1` box per the lin-ir ownership invariants — and be verified ASan-clean, since
record/array-returning intrinsics are the class that has previously produced
use-after-free / double-free bugs (cf. the `std/process` `ExecResult` and `std/regex`
`Match` notes).

### Cross-platform availability matrix

| function       | Linux | macOS | Windows | failure mode on unsupported |
|----------------|:-----:|:-----:|:-------:|-----------------------------|
| `platform`/`arch`/`cpuCount`/`pid`/`tempDir` | ✓ | ✓ | ✓ | total — never fails |
| `hostname`/`username`/`homeDir` | ✓ | ✓ | ✓ | `Error` only on a degenerate host |
| `ppid`         |  ✓   |  ✓   | ✓ (snapshot; `0` if unknown) | — |
| `uptime`       |  ✓   |  ✓   |   ✓     | `Error` if unreadable |
| `memInfo`      |  ✓   |  ✓   |   ✓     | `Error`; `free` semantics vary |
| `loadAverage`  |  ✓   |  ✓   |   ✗     | **`Error` on Windows** (no such concept) |

`loadAverage` is the one genuinely Unix-only call, and `memInfo`'s `free` field is the one
whose *meaning* drifts across platforms; both are documented above so callers treat the
`Error` / the number with the right expectations. Everything else is uniform.

### Relationship to existing modules

- **`std/env`** — `std/os` is the *machine/process* view, `std/env` is the *environment
  variable* view. `homeDir`/`tempDir` consult environment variables internally but add the
  OS-level fallback `getEnv` cannot; reading a raw variable is still `std/env`'s job. No
  overlap, no duplication.
- **`std/process`** — `std/process` runs and manages *child* processes; `std/os` reports
  on the *current* process and host. `pid()` (an OS pid) is deliberately distinct from a
  `ProcessHandle` (an opaque child id). Changing the working directory remains
  `std/process` `chdir`.
- **`std/io`** — `exit(code)` stays in `std/io`; this proposal cross-references it and
  does not add a termination call.
- **`std/async`** — `cpuCount()` is the headline integration: `threadPool(cpuCount())`
  sizes a worker pool to the host. This is the practical reason the function returns a
  plain `Int32` (not `Int32 | Error`) — it must drop straight into `threadPool` without a
  `match`, hence the "always `>= 1`" guarantee.

### Out of scope (deliberate)

- **Mutation** of any kind (set hostname, set affinity) — this module is read-only.
- **Per-process** resource accounting (RSS, CPU time of *this* process) and a full
  process table — a richer `std/sys`/observability module, if wanted, can layer on the
  same `sysinfo` backing.
- **Network interface enumeration** — belongs with `std/net`, not here.
- **A `Platform`/`Arch` enum.** The values are returned as plain `String`s from a
  documented closed set (matching Node/Go), to keep the surface dependency-free and easy
  to branch on with `==`.
