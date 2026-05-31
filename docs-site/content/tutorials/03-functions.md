# Functions

Functions are first-class values in Lin. They are defined with arrow syntax and can be passed around, stored in variables, and returned from other functions.

## Basic function syntax

```lin
val add = (a: Int32, b: Int32): Int32 =>
  a + b
```

The return type annotation is optional — Lin can infer it:

```lin
val add = (a: Int32, b: Int32) => a + b
```

Single-expression functions can go on one line.

## Multi-line function bodies

A function body with multiple statements uses indentation. The last expression is the return value:

```lin
import { print } from "std/io"

val total = (price: Float64, qty: Int32): Float64 =>
  val subtotal = price * qty
  val tax = subtotal * 0.2
  subtotal + tax
```

There is no `return` keyword — the result of the last expression is returned automatically.

## Calling functions

```lin
val result = add(3, 4)   // 7
```

## Dot application

Lin uses dot notation as an alternative calling convention where the value on the left becomes the first argument:

```lin
import { toUpper } from "std/string"

val shout = "hello".toUpper()   // "HELLO"
// same as: toUpper("hello")
```

This makes chaining natural:

```lin
import { trim, toUpper } from "std/string"

val result = "  hello world  "
  .trim()
  .toUpper()
```

## Partial application

Passing fewer arguments than a function expects returns a new function:

```lin
val add = (a: Int32, b: Int32) => a + b

val addTen = add(10)     // a function: (Int32) => Int32
val result = addTen(5)   // 15
```

## Recursion

A `val` function can reference itself by name:

```lin
import { print } from "std/io"

val factorial = (n: Int32): Int32 =>
  if n == 0 then 1
  else n * factorial(n - 1)

print(factorial(10))
```

## Tail-call optimisation

Lin performs TCO for direct self-recursive calls in tail position. Accumulator-style recursion runs in constant stack space:

```lin
val fib = (n: Int32, a: Int32, b: Int32): Int32 =>
  if n == 0 then a
  else fib(n - 1, b, a + b)

val result = fib(1000, 0, 1)   // no stack overflow
```

## First-class functions

Functions are values. Store them, pass them, return them:

```lin
import { map } from "std/array"

val double = (x: Int32) => x * 2
val nums = [1, 2, 3, 4]
val doubled = nums.map(double)   // [2, 4, 6, 8]
```

Inline (anonymous) functions work too:

```lin
val doubled = [1, 2, 3].map(x => x * 2)
```

## Closures

Functions capture their enclosing scope:

```lin
val makeAdder = (n: Int32) =>
  (x: Int32) => x + n

val addFive = makeAdder(5)
val result = addFive(3)   // 8
```

`var` bindings are captured by reference — all closures over the same `var` share the same mutable cell:

```lin
val makeCounter = () =>
  var count = 0
  () =>
    count = count + 1
    count

val counter = makeCounter()
counter()   // 1
counter()   // 2
counter()   // 3
```
