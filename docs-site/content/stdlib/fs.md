# std/fs

std/fs — filesystem read, write, and directory operations.

All operations are synchronous and blocking. Fallible calls return their value or an Error shape
(`{ "type": "error", "message": ... }`) that you narrow with `is Error`; the predicates exists,
isFile, isDir, and isSymlink are total and return a plain Boolean. The `opts` Json argument on
ls, mkdir, rm, and writeJson selects a variant via keys such as `{ recursive }`, `{ parents }`,
or `{ compact }`. For incremental reads, openRead returns a lazy byte stream.

```lin
import { readFile, writeFile, readJson, ls, mkdir, exists, isFile, isDir } from "std/fs"
```

## Reference

#### `StatOpts`

```lin
type StatOpts = { "follow": Boolean | Null }
```

Option records for the filesystem operations. Every field is optional — an omitted key reads as
`Null` (the same as not passing it), so `{}` and a partial literal like `{ "recursive": true }`
are both accepted. Pass `null` (or, for `stat`, omit the argument) to take all defaults.

#### `LsOpts`

```lin
type LsOpts = { "recursive": Boolean | Null }
```


#### `MkdirOpts`

```lin
type MkdirOpts = { "parents": Boolean | Null }
```


#### `RmOpts`

```lin
type RmOpts = { "recursive": Boolean | Null }
```


#### `WriteJsonOpts`

```lin
type WriteJsonOpts = { "compact": Boolean | Null }
```


#### `TempFileOpts`

```lin
type TempFileOpts = { "prefix": String | Null, "suffix": String | Null }
```


#### `readFile`

```lin
val readFile = (path: String): String | Error
```

Read the entire contents of a file as a UTF-8 string.
- **`path`** — the file to read.
- **Returns** the file contents as a String, or an Error if it cannot be read.

**Example:**

```lin
val content = readFile("config.txt")   // String, or Error on failure
```

#### `writeFile`

```lin
val writeFile = (path: String, content: String): Null | Error
```

Write `content` to `path`, truncating any existing file (creates it if absent).
- **`path`** — the file to write.
- **`content`** — the UTF-8 string to write.
- **Returns** Null on success, or an Error if the write fails.

#### `appendFile`

```lin
val appendFile = (path: String, content: String): Null | Error
```

Append `content` to the end of `path` (creates it if absent).
- **`path`** — the file to append to.
- **`content`** — the UTF-8 string to append.
- **Returns** Null on success, or an Error if the write fails.

#### `readLines`

```lin
val readLines = (path: String): String[] | Error
```

Read a file and split it into an array of lines (line terminators removed).
- **`path`** — the file to read.
- **Returns** a String[] of the file's lines, or an Error if it cannot be read.

#### `readJson`

```lin
val readJson = (path: String): Json | Error
```

Read a file and parse its contents as JSON.
- **`path`** — the file to read.
- **Returns** the parsed Json value, or an Error if the file cannot be read or is not valid JSON.

**Example:**

```lin
val data = readJson("config.json")   // then data["version"], or Error on failure
```

#### `writeJson`

```lin
val writeJson = (path: String, value: Json, opts: WriteJsonOpts | Null): Null | Error
```

Serialise `value` to JSON and write it to `path`, truncating any existing file.
- **`path`** — the file to write.
- **`value`** — the Json value to serialise.
- **`opts`** — accepts `{ compact }`; when true, output is minified rather than pretty-printed.
- **Returns** Null on success, or an Error if the write fails.

**Example:**

```lin
writeJson("config.json", { "version": 2 }, { "compact": true })
```

#### `exists`

```lin
val exists = (path: String): Boolean
```

Test whether anything exists at `path` (file, directory, or symlink).
- **`path`** — the path to test.
- **Returns** true if the path exists, false otherwise. Total — never fails.

#### `isFile`

```lin
val isFile = (path: String): Boolean
```

Test whether `path` exists and is a regular file (follows symlinks).
- **`path`** — the path to test.
- **Returns** true if it is a regular file, false otherwise. Total — never fails.

#### `isDir`

```lin
val isDir = (path: String): Boolean
```

Test whether `path` exists and is a directory (follows symlinks).
- **`path`** — the path to test.
- **Returns** true if it is a directory, false otherwise. Total — never fails.

#### `FileStat`

```lin
type FileStat = { "size": Int64, "modified": Int64, "created": Int64, "isFile": Boolean, "isDir": Boolean, "isSymlink": Boolean, "mode": Int32 }
```

Filesystem metadata for a path: byte size, modified/created times (Unix ms), the file-kind
flags, and the Unix permission bits in `mode` (0 on non-Unix). Every field is always present.

#### `stat`

```lin
val stat = (path: String, opts: StatOpts | Null = null): FileStat | Error
```

Read filesystem metadata for `path`, following symlinks by default (reports the target).
- **`path`** — the path to stat.
- **`opts`** — accepts `{ follow }`, default true; `{ follow: false }` reports the final symlink
  itself rather than following it (equivalent to `lstat`).
- **Returns** a `FileStat`, or an Error if the path cannot be read.

**Example:**

```lin
stat("main.lin")["size"]   // file size in bytes
```

#### `ls`

```lin
val ls = (path: String, opts: LsOpts | Null): String[] | Error
```

List the entries of a directory.
- **`path`** — the directory to list.
- **`opts`** — accepts `{ recursive }`; when true, walks the whole tree instead of one level.
- **Returns** an array of entry paths, or an Error if `path` cannot be read.

**Example:**

```lin
ls("src", {})                       // one level
```

**Example:**

```lin
ls("src", { "recursive": true })    // whole tree, relative paths
```

#### `mkdir`

```lin
val mkdir = (path: String, opts: MkdirOpts | Null): Null | Error
```

Create the directory `path`.
- **`path`** — the directory to create.
- **`opts`** — accepts `{ parents }`; when true, creates missing parent directories too (and
  succeeds if the directory already exists).
- **Returns** Null on success, or an Error (e.g. a missing parent without `parents`).

**Example:**

```lin
mkdir("output", {})
```

**Example:**

```lin
mkdir("output/reports/2024", { "parents": true })
```

#### `rm`

```lin
val rm = (path: String, opts: RmOpts | Null): Null | Error
```

Remove the file or directory at `path`.
- **`path`** — the path to remove.
- **`opts`** — accepts `{ recursive }`; when true, removes a directory and its contents (a
  non-recursive call on a non-empty directory is an Error).
- **Returns** Null on success, or an Error.

**Example:**

```lin
rm("temp.txt", {})
```

**Example:**

```lin
rm("build/", { "recursive": true })
```

#### `cp`

```lin
val cp = (src: String, dst: String): Null | Error
```

Copy a file from `src` to `dst`, overwriting `dst` if it exists.
- **`src`** — the source file.
- **`dst`** — the destination path.
- **Returns** Null on success, or an Error.

#### `mv`

```lin
val mv = (src: String, dst: String): Null | Error
```

Move (rename) a file or directory from `src` to `dst`.
- **`src`** — the source path.
- **`dst`** — the destination path.
- **Returns** Null on success, or an Error.

#### `readFileBytes`

```lin
val readFileBytes = (path: String): UInt8[] | Error
```

Read the entire contents of a file as raw bytes.
- **`path`** — the file to read.
- **Returns** a UInt8[] of the file's bytes, or an Error if it cannot be read.

#### `writeFileBytes`

```lin
val writeFileBytes = (path: String, bytes: UInt8[]): Null | Error
```

Write raw bytes to `path`, truncating any existing file (creates it if absent).
- **`path`** — the file to write.
- **`bytes`** — the bytes to write.
- **Returns** Null on success, or an Error if the write fails.

#### `writeLines`

```lin
val writeLines = (path: String, lines: String[]): Null | Error
```

Write an array of lines to `path` (each followed by a newline), truncating any existing file.
- **`path`** — the file to write.
- **`lines`** — the lines to write, without terminators.
- **Returns** Null on success, or an Error if the write fails.

#### `glob`

```lin
val glob = (pattern: String): String[] | Error
```

Expand a shell-glob pattern (`**`/`*`/`?`/`[…]`) to the matching paths, sorted.
- **`pattern`** — the glob pattern.
- **Returns** a sorted String[] of matches (an empty array when nothing matches — not a fault), or
  an Error only when the pattern is malformed.

#### `tempFile`

```lin
val tempFile = (opts: TempFileOpts | Null): String | Error
```

Create a unique, empty 0600 temp file and return its absolute path. Performs no cleanup (delete
it with `rm`); prefer withTempFile unless the file must outlive a single lexical scope.
- **`opts`** — accepts `{ prefix, suffix }` to shape the generated name (e.g. a `.ext` suffix).
- **Returns** the new file's absolute path, or an Error if it cannot be created.

#### `tempDir`

```lin
val tempDir = (): String | Error
```

Create a unique temp directory and return its absolute path. Performs no cleanup (delete it with
`rm(dir, { "recursive": true })`); prefer withTempDir for scoped use. Distinct from
`std/process.tempDir`, which reports the system temp location and creates nothing.
- **Returns** the new directory's absolute path, or an Error if it cannot be created.

#### `withTempFile`

```lin
val withTempFile = <R>(fn: (String) => R): R | Error
```

Create a fresh temp file, run `fn` with its path, then delete the file. Mirrors the
withLock/withFixture scoped-resource idiom.
- **`fn`** — receives the temp file's path; its result is returned.
- **Returns** the result of `fn`, or an Error if the temp file could not be created. Cleanup runs on
  both the success and Error paths (it is never short-circuited by an Error result).

#### `withTempDir`

```lin
val withTempDir = <R>(fn: (String) => R): R | Error
```

Create a fresh temp directory, run `fn` with its path, then recursively delete the whole tree.
- **`fn`** — receives the temp directory's path; its result is returned.
- **Returns** the result of `fn`, or an Error if the directory could not be created. Cleanup runs on
  both the success and Error paths.

#### `chmod`

```lin
val chmod = (path: String, mode: Int32): Null | Error
```

Set the Unix permission bits of `path`. A no-op on non-Unix platforms.
- **`path`** — the file to chmod.
- **`mode`** — the permission bits as a decimal Int32 (0o755 == 493).
- **Returns** Null on success, or an Error.

#### `symlink`

```lin
val symlink = (target: String, linkPath: String): Null | Error
```

Create a symbolic link at `linkPath` pointing at `target`. Argument order matches
`ln -s target linkPath`.
- **`target`** — the link's target, stored verbatim (may be relative and need not exist).
- **`linkPath`** — where to create the link.
- **Returns** Null on success, or an Error (e.g. if `linkPath` already exists).

#### `readlink`

```lin
val readlink = (path: String): String | Error
```

Read the target string stored in a symbolic link (verbatim, not resolved — use realpath for that).
- **`path`** — the symlink to read.
- **Returns** the stored target String, or an Error if `path` is not a symlink or does not exist.

#### `isSymlink`

```lin
val isSymlink = (path: String): Boolean
```

Test whether `path` is itself a symbolic link (does not follow it).
- **`path`** — the path to test.
- **Returns** true iff `path` exists and is a symlink; false for regular files, directories, and
  non-existent paths. Total — never fails.

#### `lstat`

```lin
val lstat = (path: String): FileStat | Error
```

Read filesystem metadata for `path` without following a final symlink (reports the link itself,
with `isSymlink: true`). Exactly `stat(p, { "follow": false })`.
- **`path`** — the path to stat.
- **Returns** a `FileStat`, or an Error if the path cannot be read.

#### `touch`

```lin
val touch = (path: String): Null | Error
```

Create `path` as an empty file if absent (no parent dirs created), else bump its mtime to now
without changing its contents.
- **`path`** — the file to touch.
- **Returns** Null on success, or an Error (e.g. a missing parent directory).

#### `realpath`

```lin
val realpath = (path: String): String | Error
```

Resolve `path` to its canonical, absolute, symlink-free form by touching the disk. The effectful
counterpart to the pure-string `std/path.resolve`.
- **`path`** — the path to canonicalise.
- **Returns** the canonical absolute path, or an Error if any component is absent.

#### `openRead`

```lin
val openRead = (path: String)
```

Open a file as a lazy byte stream for incremental reading. The low-level pull surface that
std/stream builds its lazy adapters on; use readChunk and closeStream to drive it.
- **`path`** — the file to open.
- **Returns** a `Stream<UInt8[]>`, or an Error if the file cannot be opened.

#### `readChunk`

```lin
val readChunk = (s: Stream): UInt8[] | Null | Error
```

Pull the next chunk of bytes from a stream.
- **`s`** — the stream to read from.
- **Returns** a `UInt8[]` chunk, Null at end-of-stream, or an Error on a read failure.

#### `closeStream`

```lin
val closeStream = (s: Stream): Null
```

Close a stream, releasing its underlying resource. Idempotent (safe to call more than once).
- **`s`** — the stream to close.
- **Returns** Null.
