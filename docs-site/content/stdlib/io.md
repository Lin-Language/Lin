# std/io

Standard input, standard output, and process control.

```lin
import { print, readLine, args, exit } from "std/io"
```

`std/io` provides functions for reading from stdin, writing to stdout and stderr, accessing command-line arguments, and exiting the process.

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `print` | `(Json) -> Null` | Write a value to stdout followed by a newline |
| `printErr` | `(Json) -> Null` | Write a value to stderr followed by a newline |
| `readLine` | `() -> String \| Null` | Read one line from stdin; `Null` on EOF |
| `readAll` | `() -> String` | Read all of stdin as one string |
| `lines` | `() -> Iterator` | Iterator over stdin lines |
| `prompt` | `(String) -> String \| Null` | Print message then read one line |
| `args` | `() -> String[]` | Command-line arguments |
| `exit` | `(Int32) -> Null` | Terminate process with exit code |

---

### `print`

```lin
val print: (value: Json) -> Null
```

Writes `value` to stdout, followed by a newline. Strings are printed without quotes; all other values are formatted as JSON.

```lin
print("hello")        // hello
print(42)             // 42
print([1, 2, 3])      // [1, 2, 3]
print({ "a": 1 })     // {"a":1}
```

---

### `printErr`

```lin
val printErr: (value: Json) -> Null
```

Same as `print` but writes to stderr.

```lin
printErr("error: file not found")
```

---

### `readLine`

```lin
val readLine: () -> String | Null
```

Reads one line from stdin, stripping the trailing newline. Returns `Null` on EOF.

```lin
val line = readLine()
match line
  is Null => print("end of input")
  else    => print("got: ${line}")
```

---

### `readAll`

```lin
val readAll: () -> String
```

Reads all of stdin and returns it as one string (including embedded newlines).

```lin
val raw = readAll()
```

---

### `lines`

```lin
val lines: () -> Iterator
```

Returns an iterator yielding one `String` per line of stdin. Terminates at EOF.

```lin
import { print, lines } from "std/io"
import { for } from "std/iter"
import { trim } from "std/string"

lines().for(line => print(line.trim()))
```

---

### `prompt`

```lin
val prompt: (message: String) -> String | Null
```

Prints `message` to stdout (without a trailing newline), then reads one line. Returns `Null` on EOF.

```lin
val name = prompt("Enter your name: ")
match name
  is Null => print("no input")
  else    => print("Hello, ${name}!")
```

---

### `args`

```lin
val args: () -> String[]
```

Returns command-line arguments starting from the first user argument (after the program name).

```lin
val arguments = args()
arguments.for(a => print(a))
```

---

### `exit`

```lin
val exit: (code: Int32) -> Null
```

Terminates the process immediately. `0` = success, non-zero = failure. Does not return.

```lin
exit(0)
```
