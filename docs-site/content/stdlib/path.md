# std/path

std/path — pure path-string manipulation. No filesystem access; works with POSIX-style paths.

Every function here is a pure string transform — nothing is read from or written to disk.
`resolve` is the one exception: it consults the current working directory to make a path
absolute. Use std/fs for actual filesystem operations.

  import { join, basename, dirname, extname, stem, normalize, resolve } from "std/path"

## Reference

#### `basename`

```lin
val basename = (p: String): String
```

The final component of a path (the file or last directory name).
- **`p`** — the path.
- **Returns** the basename, e.g. `basename("/a/b/c.txt")` is `"c.txt"`.

#### `dirname`

```lin
val dirname = (p: String): String
```

The parent-directory portion of a path.
- **`p`** — the path.
- **Returns** the dirname, e.g. `dirname("/a/b/c.txt")` is `"/a/b"`.

#### `extname`

```lin
val extname = (p: String): String
```

The file extension of a path, including the leading dot.
- **`p`** — the path.
- **Returns** the extension, e.g. `extname("c.txt")` is `".txt"` (empty if none).
- **Example:** extname("archive.tar.gz")  // ".gz"
- **Example:** extname("README")          // ""

#### `stem`

```lin
val stem = (p: String): String
```

The basename of a path with its extension removed.
- **`p`** — the path.
- **Returns** the stem, e.g. `stem("/a/c.txt")` is `"c"`.
- **Example:** stem("main.lin")        // "main"
- **Example:** stem("archive.tar.gz")  // "archive.tar"

#### `isAbsolute`

```lin
val isAbsolute = (p: String): Boolean
```

Whether a path is absolute.
- **`p`** — the path.
- **Returns** `true` if `p` is absolute, `false` if relative.

#### `normalize`

```lin
val normalize = (p: String): String
```

Collapse `.`/`..` segments and redundant separators in a path (lexical, no filesystem access).
- **`p`** — the path.
- **Returns** the normalized path.
- **Example:** normalize("a/b/../c")  // "a/c"
- **Example:** normalize("/a/./b/c")  // "/a/b/c"

#### `resolve`

```lin
val resolve = (p: String): String
```

Resolve a path to an absolute path against the current working directory.
- **`p`** — the path.
- **Returns** the absolute resolved path.

#### `join`

```lin
val join = (parts: String[]): String
```

Join path components with the platform separator and normalize the result.
- **`parts`** — the path components, in order.
- **Returns** the joined, normalized path (empty string if `parts` is empty).
- **Example:** join(["usr", "local", "bin"])  // "usr/local/bin"
- **Example:** join(["/usr", "local/bin"])    // "/usr/local/bin"

#### `split`

```lin
val split = (p: String): String[]
```

Split a path into its `/`-separated components.
- **`p`** — the path.
- **Returns** an array of the path segments.

#### `relative`

```lin
val relative = (_: String, to: String): String
```

The path of `to` relative to a base path.
- **`to`** — the target path.
- **Returns** the relative path (currently returns `to` unchanged).
