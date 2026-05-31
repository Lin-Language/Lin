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

To partially apply a function — supply some arguments now and get back a function
awaiting the rest — add an **explicit trailing comma** after the arguments you provide:

```lin
val add = (a: Int32, b: Int32) => a + b

val addTen = add(10,)    // a function: (Int32) => Int32
val result = addTen(5)   // 15
```

The trailing comma is what marks the call as partial. Without it, `add(10)` is a
complete call that's missing an argument — an error, unless the omitted parameter
has a default value.

## Optional parameters

A parameter can declare a default with `= expr` after its type. Such a parameter is
optional: a complete call may leave it out, and the default fills in.

```lin
import { print } from "std/io"

val greet = (name: String, greeting: String = "Hello"): String =>
  "${greeting}, ${name}!"

print(greet("World"))         // Hello, World!
print(greet("World", "Hi"))   // Hi, World!
```

Optional parameters must come last — once a parameter has a default, every parameter
after it must too. A default can reference parameters declared before it, so defaults
can chain:

```lin
val box = (w: Int32, h: Int32 = w, area: Int32 = w * h): Int32 => area

box(4)        // h = 4, area = 16
box(4, 3)     // area = 12
box(4, 3, 99) // area = 99
```

A complete call still has to supply the required (non-defaulted) parameters; omitting
those is an error. And remember the distinction from partial application: `greet("World")`
*fills* the default, while `greet("World",)` — with the trailing comma — *curries*,
returning a `(String) => String` that still expects `greeting`.

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

## Generic functions

A function can declare type parameters in angle brackets before its argument list. They let one function work over many types while keeping the relationship between argument and result types precise:

```lin
import { print } from "std/io"
import { toString } from "std/string"

val identity = <T>(x: T): T => x

print(toString(identity(42)))   // 42
print(identity("hello"))        // hello
```

Type parameters are inferred from the arguments at each call site — you don't write them explicitly when calling. Use several when the types relate to each other:

```lin
val pair = <A, B>(a: A, b: B): { "first": A, "second": B } =>
  { "first": a, "second": b }

val firstOf = <T>(xs: T[]): T => xs[0]

print(firstOf([10, 20, 30]))    // 10
print(firstOf(["a", "b"]))      // a
```

Here `firstOf` guarantees its result has the same element type as the array it was given — an `Int32[]` in, an `Int32` out. See the [Types reference](/reference/types.html#generic-types) for generic *type* declarations (`type Result<T, E> = ...`) and variance.
