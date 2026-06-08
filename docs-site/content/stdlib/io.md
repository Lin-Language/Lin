# std/io

std/io — standard input, standard output, and process control.

Functions for reading from stdin, writing to stdout/stderr, accessing the command-line
arguments, and exiting the process. `print` stringifies any value (strings unquoted, everything
else as JSON); the read functions return `String | Null` so EOF narrows with a plain `== null`.

  import { print, readLine, args, exit } from "std/io"

## Reference

#### `readLine`

```lin
val readLine = (): Json
```

Read one line from standard input (without the trailing newline).
- **Returns** the line as a `String`, or `null` at end of input.

#### `readAll`

```lin
val readAll = (): String
```

Read all of standard input to end of stream.
- **Returns** the entire input as a single `String`.

#### `lines`

```lin
val lines = (): String[]
```

Read all of standard input as lines.
- **Returns** an array of the input lines (newlines stripped).

#### `print`

```lin
val print = (x: Json): Null
```

Print a value to standard output, followed by a newline.
- **`x`** — the value to print (stringified).
- **Example:** print("hello")     // hello
- **Example:** print([1, 2, 3])   // [1, 2, 3]
- **Example:** print({ "a": 1 })  // {"a":1}

#### `printErr`

```lin
val printErr = (x: Json): Null
```

Print a value to standard error, followed by a newline.
- **`x`** — the value to print (stringified).

#### `args`

```lin
val args = (): String[]
```

The process command-line arguments.
- **Returns** an array of the argument strings.

#### `prompt`

```lin
val prompt = (message: String): Json
```

Print a prompt and read one line of input.
- **`message`** — the prompt to print before reading.
- **Returns** the line read, or `null` at end of input.

#### `exit`

```lin
val exit = (code: Int32): Null
```

Terminate the process with the given exit code.
- **`code`** — the process exit status (0 = success).

#### `stdinStream`

```lin
val stdinStream = (): Stream
```

Wrap the process's standard input as a lazy byte `Stream<UInt8[]>` (streams brief §4).
- **Returns** a `Stream` that pulls from stdin until EOF; pair with `lines`/`map`/… from std/stream.
