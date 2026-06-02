# Hello World & I/O

This tutorial walks through your first Lin program, input and output, and string interpolation.

## Hello World

Every Lin program that produces output imports `print` from `std/io`:

```lin
import { print } from "std/io"

print("hello, world!")
```

Save this as `hello.lin`, then compile and run:

```
lin build hello.lin -o hello
./hello
```

`print` accepts any value — strings, numbers, arrays, objects. Strings are printed without quotes; everything else is formatted as JSON.

## String interpolation

Lin does not support `+` for string concatenation. Instead, use `${expr}` inside a double-quoted string:

```lin
import { print } from "std/io"

val name = "Lin"
val version = 1
print("Welcome to ${name} v${version}!")
```

Any expression can appear inside `${...}`:

```lin
import { print } from "std/io"

val x = 6
val y = 7
print("${x} times ${y} is ${x * y}")
```

## Reading a line from stdin

`readLine` reads one line and returns `String | Null` (null on EOF):

```lin
import { print, readLine } from "std/io"

print("What is your name?")
val input = readLine()
match input
  is Null => print("no input provided")
  else    => print("Hello, ${input}!")
```

The `match` / `is` / `else` pattern is the standard way to inspect union values. More on that in the [pattern matching tutorial](/tutorials/05-pattern-matching.html).

## Reading all lines

`lines` returns an iterator over stdin lines:

```lin
import { print, lines } from "std/io"
import { for } from "std/iter"

lines().for(line =>
  print("got: ${line}")
)
```

## Writing to stderr

`printErr` works like `print` but writes to stderr:

```lin
import { printErr } from "std/io"

printErr("warning: file not found")
```

## Command-line arguments

`args` returns the command-line arguments as an array of strings:

```lin
import { print, args } from "std/io"
import { length } from "std/array"
import { for } from "std/iter"

val arguments = args()
print("got ${arguments.length()} arguments")
arguments.for(a => print("  ${a}"))
```

## Exiting with a code

```lin
import { exit } from "std/io"

exit(0)    // success
exit(1)    // failure
```

`exit` terminates the process immediately and does not return.
