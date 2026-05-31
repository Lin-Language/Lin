# std/path

Pure path string manipulation. No filesystem access. Works with both POSIX and Windows paths.

```lin
import { join, basename, dirname, extname, stem, normalize, resolve, relative, isAbsolute, split } from "std/path"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `basename` | `(String) -> String` | Final component of path |
| `dirname` | `(String) -> String` | All components except the last |
| `extname` | `(String) -> String` | Extension including dot, or `""` |
| `isAbsolute` | `(String) -> Boolean` | True if path is absolute |
| `join` | `(String[]) -> String` | Join segments with OS separator |
| `normalize` | `(String) -> String` | Resolve `..` and `.` |
| `relative` | `(String, String) -> String` | Relative path from one location to another |
| `resolve` | `(String) -> String` | Resolve to absolute path using cwd |
| `split` | `(String) -> String[]` | Split path into components |
| `stem` | `(String) -> String` | Basename without extension |

---

### `join`

```lin
join(["usr", "local", "bin"])   // "usr/local/bin"
join(["/usr", "local/bin"])     // "/usr/local/bin"
```

Takes an **array** of strings (not variadic).

---

### `basename`

```lin
basename("/usr/local/bin/lin")   // "lin"
basename("src/main.lin")         // "main.lin"
```

---

### `dirname`

```lin
dirname("/usr/local/bin/lin")   // "/usr/local/bin"
dirname("src/main.lin")         // "src"
dirname("main.lin")             // "."
```

---

### `extname`

```lin
extname("main.lin")         // ".lin"
extname("archive.tar.gz")   // ".gz"
extname("README")           // ""
```

---

### `stem`

```lin
stem("main.lin")         // "main"
stem("archive.tar.gz")   // "archive.tar"
```

---

### `normalize`

```lin
normalize("a/b/../c")    // "a/c"
normalize("/a/./b/c")    // "/a/b/c"
normalize("a//b")        // "a/b"
```

---

### `resolve`

```lin
// assuming cwd = "/home/user/project"
resolve("src/main.lin")   // "/home/user/project/src/main.lin"
resolve("/etc/hosts")     // "/etc/hosts"
```

---

### `relative`

```lin
relative("/usr/local", "/usr/local/bin/lin")   // "bin/lin"
relative("/usr/local/bin", "/usr/share")       // "../../share"
```

---

### `isAbsolute`

```lin
isAbsolute("/usr/local")    // true
isAbsolute("src/main.lin")  // false
```

---

### `split`

```lin
split("/usr/local/bin")   // ["", "usr", "local", "bin"]
split("src/main.lin")     // ["src", "main.lin"]
```
