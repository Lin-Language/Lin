# std/fs

Filesystem read, write, and directory operations. All operations are synchronous and blocking.

```lin
import { readFile, writeFile, readJson, ls, mkdir, exists, isFile, isDir } from "std/fs"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `appendFile` | `(String, String) -> Null \| Error` | Append string to file |
| `cp` | `(String, String) -> Null \| Error` | Copy a file |
| `exists` | `(String) -> Boolean` | Test whether path exists |
| `isDir` | `(String) -> Boolean` | True if path is a directory |
| `isFile` | `(String) -> Boolean` | True if path is a regular file |
| `ls` | `(String, Json) -> String[] \| Error` | List directory; `{ recursive: true }` for deep listing |
| `mkdir` | `(String, Json) -> Null \| Error` | Create directory; `{ parents: true }` for `-p` |
| `mv` | `(String, String) -> Null \| Error` | Move or rename a file |
| `readFile` | `(String) -> String \| Error` | Read file as UTF-8 string |
| `readFileBytes` | `(String) -> UInt8[] \| Error` | Read file as a raw byte buffer |
| `readJson` | `(String) -> Json \| Error` | Read and parse file as JSON |
| `readLines` | `(String) -> String[] \| Error` | Read file lines into an array |
| `rm` | `(String, Json) -> Null \| Error` | Remove file; `{ recursive: true }` for directory |
| `stat` | `(String) -> FileStat \| Error` | File metadata |
| `writeFile` | `(String, String) -> Null \| Error` | Write string to file |
| `writeFileBytes` | `(String, UInt8[]) -> Null \| Error` | Write a raw byte buffer to file |
| `writeJson` | `(String, Json, Json) -> Null \| Error` | Write JSON; `{ compact: true }` for minified |
| `writeLines` | `(String, String[]) -> Null \| Error` | Write lines to file |

## FileStat type

```lin
type FileStat = {
  "size":     Int64,    // bytes
  "modified": Int64,    // Unix timestamp ms
  "created":  Int64,    // Unix timestamp ms
  "isFile":   Boolean,
  "isDir":    Boolean,
  "mode":     Int32     // Unix permission bits
}
```

---

### `readFile`

```lin
val content = readFile("config.txt")
match content
  has { "type": "failure", error } => print("error: ${error}")
  else => print(content)
```

---

### `writeFile`

```lin
val result = writeFile("output.txt", "hello world\n")
match result
  has { "type": "failure", error } => print("write failed: ${error}")
  else => null
```

---

### `readJson`

```lin
val data = readJson("config.json")
match data
  has { "type": "failure", error } => print("parse error: ${error}")
  else => print(data["version"])
```

---

### `ls`

```lin
// List directory
val files = ls("src", {})

// Recursive listing (returns relative paths)
val all = ls("src", { "recursive": true })
```

---

### `mkdir`

```lin
mkdir("output", {})
mkdir("output/reports/2024", { "parents": true })
```

---

### `exists` / `isFile` / `isDir`

```lin
if exists("data.json") then
  val data = readJson("data.json")
  ...
else null
```

---

### `cp` / `mv` / `rm`

```lin
cp("src.txt", "dst.txt")
mv("old.txt", "new.txt")
rm("temp.txt", {})
rm("build/", { "recursive": true })
```

---

### `stat`

```lin
val info = stat("main.lin")
print("size: ${info["size"]} bytes")
```
