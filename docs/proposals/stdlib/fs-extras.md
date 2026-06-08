# std/fs extras — design proposal

## Status: proposal (enriches std/fs)

The existing [`std/fs`](../../STDLIB.md#stdfs) covers the everyday whole-file surface —
`readFile`/`writeFile`, `readJson`/`writeJson`, `ls`, `mkdir`, `rm`, `cp`, `mv`, `stat`,
the byte-buffer pair, and the low-level stream-pull primitives. What it lacks is the
*shell-and-scripting* layer that Python (`glob`, `tempfile`, `shutil`, `os`), Go
(`path/filepath.Glob`, `os.MkdirTemp`/`CreateTemp`, `os.Symlink`), and Node (`fs.glob`,
`fs.mkdtemp`, `fs.symlink`, `fs.watch`) all ship and that real Lin programs reach for the
moment they leave the "read a config, write a report" happy path: matching files by
pattern, scratch files that clean themselves up, permission and symlink handling, and
change notification.

This proposal adds those as **additions to the existing `std/fs` module** (no new module),
plus one cross-reference to `std/path`. It keeps the established conventions verbatim:
fallible calls return `T | Error` (the canonical `{ "type": "error", "message": String }`,
detectable with `is Error`); predicates return a bare `Boolean`; option bags are `{ option }`
objects (`{ recursive }`, `{ parents }`, and the new `{ follow }`); all paths and patterns
are codepoint-aware strings. The headline items are `glob`, scoped temp files/dirs, `chmod`
+ a `mode` already in `FileStat`, and the symlink trio. File **watching** is the heaviest
item and is proposed as a `Stream<FsEvent>` but explicitly **deferred** — see [Defer](#defer).

---

## std/fs (additions)

Import (additions only):

```txt
import { glob, withTempFile, withTempDir, tempFile, tempDir, chmod,
         symlink, readlink, isSymlink, lstat, touch, realpath } from "std/fs"
```

### Updated `FileStat`

`FileStat` already carries `mode` (the Unix permission bits), so no field needs adding for
`chmod`. The one addition is a symlink discriminator so `stat`/`lstat` results are
self-describing:

```txt
type FileStat = {
  "size":      Int64,
  "modified":  Int64,
  "created":   Int64,
  "isFile":    Boolean,
  "isDir":     Boolean,
  "isSymlink": Boolean,    // NEW: true only from lstat (or stat with { follow: false })
  "mode":      Int32
}
```

`isSymlink` is `false` from a following `stat` (which resolves the link and reports the
target), and `true` from `lstat` / `stat(path, { follow: false })` when `path` is itself a
symlink. Adding a field is width-compatible with existing `stat` consumers (extra keys are
permitted), so this is a non-breaking change.

### New functions

| Function | Signature | Summary |
| --- | --- | --- |
| [`glob`](#glob) | `(String) -> String[] \| Error` | Expand a shell glob pattern (`**`/`*`/`?`/`[…]`) to matching paths |
| [`withTempFile`](#withtempfile) | `<R>((String) -> R) -> R \| Error` | Create a temp file, run `fn` with its path, delete it after |
| [`withTempDir`](#withtempdir) | `<R>((String) -> R) -> R \| Error` | Create a temp dir, run `fn` with its path, recursively delete after |
| [`tempFile`](#tempfile) | `(opts: Json) -> String \| Error` | Create a unique temp file, return its path (caller deletes) |
| [`tempDir`](#tempdir) | `() -> String \| Error` | Create a unique temp directory, return its path (caller deletes) |
| [`chmod`](#chmod) | `(String, Int32) -> Null \| Error` | Set Unix permission bits on `path` |
| [`symlink`](#symlink) | `(String, String) -> Null \| Error` | Create a symbolic link at `linkPath` pointing at `target` |
| [`readlink`](#readlink) | `(String) -> String \| Error` | Read the target of a symbolic link |
| [`isSymlink`](#issymlink) | `(String) -> Boolean` | True if `path` is itself a symbolic link |
| [`lstat`](#lstat) | `(String) -> FileStat \| Error` | Like `stat`, but does not follow a final symlink |
| [`touch`](#touch) | `(String) -> Null \| Error` | Create `path` if absent; otherwise bump its mtime |
| [`realpath`](#realpath) | `(String) -> String \| Error` | Resolve a path to a canonical, symlink-free absolute path |

---

### glob

```txt
val glob: (pattern: String) -> String[] | Error
```

Returns every path on disk that matches the shell-glob `pattern`, sorted, as an array of
strings. The pattern dialect is the standard one:

- `*` matches any run of characters within a single path segment (does **not** cross `/`).
- `**` matches any number of path segments, including zero (recursive descent).
- `?` matches a single character within a segment.
- `[abc]` / `[a-z]` matches one character from a set or range; `[!…]` / `[^…]` negates.

Returns an empty array (`[]`) when nothing matches — a no-match is an ordinary outcome, not
a fault. Returns an `Error` only when the pattern is malformed (e.g. an unterminated
`[`-class). Hidden entries (dotfiles) are not matched by a leading `*` unless the pattern
itself begins with `.`, matching shell behaviour.

```txt
glob("src/*.lin")              // ["src/main.lin", "src/util.lin"]
glob("src/**/*.lin")           // every .lin under src/, at any depth
glob("data/report-???.csv")    // ["data/report-001.csv", ...]
glob("logs/[0-9]*.log")        // logs whose name starts with a digit
glob("missing-dir/*")          // []  (no match, not an error)
```

`glob` relates directly to `ls(path, { recursive: true })`: `ls {recursive}` enumerates
*everything* under a directory and hands back relative paths, whereas `glob` filters by a
pattern and hands back paths relative to the cwd as written in the pattern. Reach for `ls`
when you want the whole tree and will filter in Lin; reach for `glob` when the selection is
expressible as a pattern (the common case, and far cheaper than walking + filtering for
patterns like `src/**/*.test.lin`).

```txt
// equivalent intent, glob is the idiomatic form:
ls("src", { recursive: true }).filter((p: String): Boolean => p.endsWith(".lin"))
glob("src/**/*.lin")
```

---

### withTempFile

```txt
val withTempFile: <R>(fn: (path: String) -> R) -> R | Error
```

Creates a fresh, uniquely-named temporary file inside the system temp directory, invokes
`fn` with its absolute path, and **deletes the file when `fn` returns** — including when
`fn` returns an `Error` value. Returns whatever `fn` returns, or an `Error` if the temp file
could not be created. This is the **recommended** way to use a scratch file: it follows the
same scoped-resource idiom as [`withLock`](../../STDLIB.md#shared--get--set--withlock) and
the `withFixture` test helper, so cleanup is automatic and leak-free even on the error path.

```txt
val sorted = withTempFile((tmp: String): String | Error => {
  writeLines(tmp, lines)
  // ... shell out to sort, re-read, etc. while tmp exists ...
  readFile(tmp)
})
// tmp is already gone here, whether sorted is a String or an Error
```

The file is created empty with restrictive (owner-only, `0600`) permissions and is
guaranteed not to collide with a concurrent caller. The path passed to `fn` is absolute, so
it is safe regardless of the program's cwd.

> Cleanup semantics: `withTempFile` deletes the file *by the path it created*. If `fn`
> renames or moves the file (e.g. `mv(tmp, "final.txt")`), there is nothing at the original
> path to delete and the destination is left in place — this is the intended way to
> "promote" a temp file to a permanent one atomically.

---

### withTempDir

```txt
val withTempDir: <R>(fn: (path: String) -> R) -> R | Error
```

Creates a fresh, uniquely-named temporary **directory**, invokes `fn` with its absolute
path, and **recursively deletes the directory and everything in it when `fn` returns**
(equivalent to `rm(dir, { recursive: true })`), on both the success and error paths. Returns
`fn`'s result, or an `Error` if the directory could not be created. Use this when a task
needs several scratch files together (an unpacked archive, a build sandbox, generated
fixtures).

```txt
withTempDir((dir: String): Null | Error => {
  val payload = join([dir, "payload.json"])
  writeJson(payload, data, {})
  unpackInto(dir)
  validate(dir)
})
// the whole directory tree is removed here
```

---

### tempFile

```txt
val tempFile: (opts: Json) -> String | Error
```

Creates a unique, empty temporary file and returns its absolute path **without** registering
any cleanup — the caller is responsible for deleting it (with `rm`). Prefer
[`withTempFile`](#withtempfile) unless the file genuinely must outlive a single lexical
scope (e.g. it is handed to a long-lived subprocess or returned from a function). Pass
`{ "prefix": "..." }` to make the generated name easier to recognise, and/or
`{ "suffix": ".json" }` to give it an extension.

```txt
tempFile({})                                  // "/tmp/lin-7f3a9c1e"
tempFile({ "prefix": "report-" })             // "/tmp/report-1b2c3d4e"
tempFile({ "prefix": "out-", "suffix": ".csv" })  // "/tmp/out-a1b2c3.csv"
```

The file is created with owner-only (`0600`) permissions. Because the path is returned to
Lin as a plain string, the **caller owns the lifecycle**; forgetting to `rm` it leaks a file
on disk (not memory), which is why the scoped helper is recommended.

---

### tempDir

```txt
val tempDir: () -> String | Error
```

Creates a unique temporary **directory** and returns its absolute path, without registering
cleanup — the caller deletes it with `rm(dir, { recursive: true })`. Prefer
[`withTempDir`](#withtempdir) for scoped use.

```txt
val work = tempDir()              // "/tmp/lin-9a8b7c6d"
// ... use work ...
rm(work, { recursive: true })
```

> Do not confuse this with `std/os.tempDir`, which *reports the location of the system
> temp directory* (the value of `TMPDIR`/`/tmp`, like Python's `tempfile.gettempdir()`)
> without creating anything. `std/fs.tempDir()` **creates and returns a new unique directory
> inside** that location. See [Implementation notes](#implementation-notes) for the
> cross-module relationship.

---

### chmod

```txt
val chmod: (path: String, mode: Int32) -> Null | Error
```

Sets the Unix permission bits of `path` to `mode`, which is interpreted the same way as the
`mode` field of [`FileStat`](#updated-filestat) (e.g. `0o644`, `0o755`). Returns `Null` on
success, `Error` if `path` does not exist or the change is not permitted. On non-Unix
platforms this is a no-op that returns `Null` (consistent with `mode` being `0` in `stat`
there).

```txt
chmod("build/run.sh", 0o755)     // make executable
chmod("secrets.env", 0o600)      // owner read/write only
```

Lin has no octal literal sugar; write the mode as a decimal `Int32` (`0o755` == `493`) or
compute it. A future `std/fs` constants set (`MODE_RWX_USER`, …) could improve readability,
but is out of scope here.

---

### symlink

```txt
val symlink: (target: String, linkPath: String) -> Null | Error
```

Creates a symbolic link at `linkPath` whose contents are `target` (the path the link points
at). `target` is stored verbatim and may be relative — it is **not** resolved or required to
exist (dangling links are legal, matching POSIX `symlink(2)` and Node `fs.symlink`).
Argument order is `(target, linkPath)`, the same order as the shell `ln -s target linkPath`
and Node `fs.symlink(target, path)`. Returns an `Error` if `linkPath` already exists.

```txt
symlink("releases/v2.3.0", "current")      // current -> releases/v2.3.0
readlink("current")                         // "releases/v2.3.0"
```

---

### readlink

```txt
val readlink: (path: String) -> String | Error
```

Returns the target string stored in the symbolic link at `path` — i.e. the value that was
passed as `target` to [`symlink`](#symlink), **not** a resolved absolute path (use
[`realpath`](#realpath) for that). Returns an `Error` if `path` is not a symlink or does not
exist.

```txt
readlink("current")     // "releases/v2.3.0"  (verbatim, may be relative)
realpath("current")     // "/srv/app/releases/v2.3.0"  (fully resolved)
```

---

### isSymlink

```txt
val isSymlink: (path: String) -> Boolean
```

Returns `true` if `path` exists and is itself a symbolic link (it does **not** follow the
link). Returns `false` for regular files, directories, broken-but-present links' targets,
and non-existent paths — mirroring the `isFile` / `isDir` predicates, which already follow
symlinks. This is the cheap, total counterpart to reading `isSymlink` off an
[`lstat`](#lstat) result.

```txt
isSymlink("current")     // true
isSymlink("releases")    // false (a real directory)
isSymlink("missing")     // false
```

---

### lstat

```txt
val lstat: (path: String) -> FileStat | Error
```

Identical to [`stat`](../../STDLIB.md#stat), except that when `path` is a symbolic link it
reports metadata about the **link itself** (size = length of the target string,
`isSymlink: true`) rather than following it to its target. For every non-link path,
`lstat` and `stat` return the same thing.

This is the standard `stat` vs `lstat` split (POSIX, Go, Node). For callers who prefer an
option bag over a second function name, `stat` also accepts an optional `{ follow }` flag:

```txt
val link = stat("current")                       // follows: target's metadata
val raw  = lstat("current")                       // does not follow
val raw2 = stat("current", { "follow": false })   // equivalent to lstat
```

Both spellings are provided deliberately: `lstat` is the name every Unix programmer reaches
for and keeps the surface discoverable, while `stat(path, { follow })` keeps the option-bag
convention internally consistent. `lstat(p)` is exactly `stat(p, { follow: false })`. (Note:
this makes `stat` take an optional second argument — a width-compatible signature change that
existing one-argument `stat` calls continue to satisfy by passing no opts.)

---

### touch

```txt
val touch: (path: String) -> Null | Error
```

If `path` does not exist, creates it as an empty file (creating no parent directories — use
`mkdir(dirname(path), { parents: true })` first if needed). If it already exists, updates its
modification time to now without changing its contents. Mirrors the `touch(1)` utility.
Returns an `Error` if a parent directory is missing.

```txt
touch("build/.stamp")     // create-or-bump a marker file
```

---

### realpath

```txt
val realpath: (path: String) -> String | Error
```

Resolves `path` to its canonical absolute form: makes it absolute, normalises `.`/`..`, and
**resolves every symbolic link along the way** by touching the filesystem. Returns an
`Error` if any component does not exist. This is the filesystem-touching counterpart to the
pure-string [`std/path.resolve`](../../STDLIB.md#resolve): `resolve` joins against the cwd
and normalises *textually* (it never reads the disk and never follows links), whereas
`realpath` returns the true on-disk canonical path.

```txt
// current -> releases/v2.3.0, cwd = /srv/app
resolve("current")     // "/srv/app/current"            (textual, link not followed)
realpath("current")    // "/srv/app/releases/v2.3.0"    (link resolved on disk)
```

Use `std/path.resolve` when you only need an absolute string and the target may not exist
yet; use `realpath` when you need the canonical identity of an existing file (e.g. to detect
that two paths are the same file through different links).

---

## Defer

### watch — file-change notification as `Stream<FsEvent>`

```txt
// PROPOSED SHAPE (deferred — do not implement in the first cut)
type FsEvent = {
  "kind": String,    // "created" | "modified" | "removed" | "renamed"
  "path": String     // the affected path
}

val watch: (path: String, opts: Json) -> Stream<FsEvent> | Error
```

Watching a path (or a directory tree, via `{ "recursive": true }`) for changes is the
single heaviest item in this proposal, and it should **not** ship with the rest. The design
question is the shape, and the recommendation is a **`Stream<FsEvent>`, not a
`watch(path, callback)` callback**:

- Lin's whole ethos is lazy pull-streams (`openRead` returns `Stream<UInt8[]>`, the
  `std/stream` adapters, `Stream<T>` as a first-class opaque handle). A filesystem watcher
  is *exactly* an unbounded source of events — the canonical thing a stream models. Exposing
  it as a stream lets callers reuse the existing `map`/`filter`/`for`/`take` adapters and the
  affine consume-once discipline, rather than inventing a parallel callback-registration API
  with its own deregistration story.
- A raw `(FsEvent) -> Null` callback would need a separate `Watcher` handle with a `close`
  method to stop it, re-deriving the resource lifecycle that streams already encode (close
  the stream → stop watching). It also collides with the known closure-capture-of-`var`
  escape bugs (see the worker/obj-literal captured-var notes), since a long-lived callback is
  precisely an escaping closure.
- The async story is then automatic: a watch stream is consumed like any other stream, and
  can be driven on a `std/async` worker or bridged into a `std/event` emitter
  (`Stream<FsEvent>` → `emitter<FsEvent, …>`) if the program is event-oriented, **without
  `std/fs` itself taking a dependency on `std/event`**. Keeping `std/fs` stream-shaped rather
  than emitter-shaped is the loose-coupling choice.

It is deferred (not just sketched) for three concrete reasons:

1. **Platform binding weight.** A correct implementation needs the `notify` crate
   (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows) wired into the
   runtime as a long-lived background source — materially more runtime surface than the
   synchronous syscalls everything else here maps to.
2. **Stream-as-live-source plumbing.** Every existing `Stream<T>` in Lin is a *pull* over a
   finite-ish source (a file, an array). A watcher is an open-ended *push* source that must
   be buffered into the pull model (a bounded channel the runtime fills and the stream
   drains). That bridge is new infrastructure and wants its own design pass.
3. **Semantics are genuinely fiddly.** Event coalescing, rename-as-(remove,create) on some
   platforms, editor atomic-save patterns, and recursive-watch descriptor limits all need
   pinning down. None of that should block the high-value, low-risk items above.

Ship the rest now; revisit `watch` once the live-push-source stream bridge exists.

### du / recursive size — optional, low priority

A recursive directory-size helper (`du(path) -> Int64 | Error`, summing `stat["size"]` over
the tree) is trivially expressible today in pure Lin over `ls(path, { recursive: true })` +
`stat`, so it earns its place only as a convenience. It can be added later as a thin Lin
wrapper with no new intrinsic; it is not part of this proposal's core.

---

## Implementation notes

### Intrinsics (must be Rust)

The new filesystem operations are thin wrappers over Rust std / crates, declared in
`stdlib/fs.lin` alongside the existing `lin_fs_*` block as `import foreign "lin-runtime"`:

```txt
import foreign "lin-runtime"
  val lin_fs_glob:        (String) => Json          // String[] | Error (bad pattern => Error)
  val lin_fs_temp_file:   (String, String) => Json  // (prefix, suffix) => path String | Error
  val lin_fs_temp_dir:    () => Json                // path String | Error
  val lin_fs_chmod:       (String, Int32) => Json   // Null | Error
  val lin_fs_symlink:     (String, String) => Json  // (target, linkPath) => Null | Error
  val lin_fs_readlink:    (String) => Json          // String | Error
  val lin_fs_is_symlink:  (String) => Boolean
  val lin_fs_lstat:       (String) => Json          // FileStat | Error
  val lin_fs_touch:       (String) => Json          // Null | Error
  val lin_fs_realpath:    (String) => Json          // String | Error
```

Crate choices:

- **`glob`** → the [`glob`](https://docs.rs/glob) crate (handles `**`/`*`/`?`/`[…]`,
  returns results sorted; map a `PatternError` to the canonical `Error` value and an I/O
  error during iteration to a skipped entry, matching shell tolerance).
- **`tempFile`/`tempDir`** → the [`tempfile`](https://docs.rs/tempfile) crate's
  `NamedTempFile`/`TempDir` to get the atomic, collision-free, `0600` creation, but
  immediately `.keep()` / `.into_path()` so the path *outlives the Rust handle* — the Lin
  side owns the lifecycle, the runtime must not auto-delete on drop. (This is the key
  contrast with the scoped helpers below.)
- **`chmod`/`symlink`/`readlink`/`lstat`/`touch`/`realpath`** → `std::fs` / `std::os::unix`
  directly (`set_permissions` + `PermissionsExt`, `symlink`, `read_link`, `symlink_metadata`,
  `OpenOptions::create`, `canonicalize`). No extra crate. On non-Unix, `chmod` is a no-op
  returning `Null` and the symlink/`mode` paths degrade gracefully as the existing `mode: 0`
  convention already establishes.

Every fallible intrinsic returns the canonical `{ "type": "error", "message": String }` value
on failure (built by the same helper the existing `lin_fs_*` calls use), so `is Error`
detection and the `T | Error` return types are uniform with the rest of the module.

### The scoped helpers (`withTempFile` / `withTempDir`) are pure Lin

`withTempFile` and `withTempDir` are **not** intrinsics — they are pure-Lin wrappers over the
raw `tempFile`/`tempDir` plus `rm`, exactly mirroring how `withLock` wraps `get`/`set`:

```txt
export val withTempFile = <R>(fn: (String) -> R): R | Error =>
  match tempFile({})
    is Error => tempFile({})            // propagate the creation error
    else =>
      val path = tempFile({})
      val result = fn(path)
      val _ = rm(path, {})              // delete regardless of result (incl. Error)
      result
```

This keeps the cleanup policy in Lin where it is auditable, and means the raw
`tempFile`/`tempDir` (caller-deletes) and the scoped helpers (auto-delete) share one
intrinsic. The deliberate design call is to **recommend the scoped helpers and provide the
raw ones as the escape hatch**, rather than make the raw form register a finalizer — Lin has
no `defer`/destructor for arbitrary values, and a stream-style closeable handle would be
overkill for a path string. Implementers should confirm the helper releases its temps even
when `fn` returns an `Error` value (the cleanup must run before `result` is returned, not be
short-circuited by the `Error`).

### `FileStat` and the `{ follow }` option

`mode` is already in `FileStat`, so `chmod` needs no schema change. The only addition is
`isSymlink`, populated by `lstat` (`true` for a link) and by a following `stat` (`false`,
since it reports the target). Implement `stat`'s new optional `opts` argument the same way
the existing `ls`/`mkdir`/`rm` do — branch on `opts["follow"] == false` to call
`lin_fs_lstat`, else `lin_fs_stat` — so the option-bag plumbing is identical to what is
already in `fs.lin`. `lstat(p)` is the named alias for `stat(p, { "follow": false })`.

### Cross-module relationships

- **`std/os.tempDir`** (separately proposed) returns the *location* of the system temp
  directory (`$TMPDIR` or `/tmp`) and creates nothing. **`std/fs.tempDir()`** creates a new
  unique directory **inside** that location and returns its path. The runtime intrinsic for
  `std/fs` should resolve the base directory through the same OS call `std/os.tempDir` uses,
  so the two agree on the base; document the distinction prominently since the names collide.
- **`std/path.resolve`** is pure-string and never touches disk; **`std/fs.realpath`** is the
  disk-touching, link-resolving canonicaliser. They are intentionally split along the
  pure-vs-effectful line that already separates `std/path` (no filesystem access) from
  `std/fs`. `realpath` lives in `std/fs`, not `std/path`, precisely because it does I/O.

### Generics / RC / compiler constraints

`withTempFile`/`withTempDir` are generic over the return type `R` (`<R>((String) -> R) -> R |
Error`). This is argument-driven inference from the closure's return type, which Lin supports;
no turbofish is needed since `fn` is a concrete witness argument. The remaining functions are
monomorphic over `String`/`Int32`/`FileStat` and pose no inference difficulty. The two
record-returning intrinsics (`lstat`, and the `FileStat` from a non-following `stat`) and the
array-returning `glob` must follow the owned-result RC contract — a fresh +1 box for the
returned `FileStat` / `String[]`, per the lin-ir ownership invariants — and be verified
ASan-clean, since record- and array-returning intrinsics are the established source of
use-after-free / double-free bugs in this codebase.

### Out of scope (deliberate)

- **`watch`** — deferred; see [Defer](#defer). Needs the `notify` crate and a live-push →
  pull-stream bridge that does not exist yet.
- **Octal literal syntax** for `chmod` modes — modes are passed as decimal `Int32`. A
  constants set is a possible later nicety, not part of this proposal.
- **`du` / recursive size** — expressible in pure Lin today over `ls {recursive}` + `stat`;
  optional convenience only.
- **Watching/globbing over `Stream<T>`** beyond the deferred `watch` — `glob` is eager and
  returns a finite array; a streaming glob is not proposed.
