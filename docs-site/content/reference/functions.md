# Functions Reference

## Function expression syntax

```lin
val name = (param1: Type1, param2: Type2): ReturnType =>
  body
```

Return type is optional and inferred where possible. Single-expression bodies go on the same line:

```lin
val add = (a: Int32, b: Int32) => a + b
```

Multi-expression bodies use indentation:

```lin
val process = (x: Int32): Int32 =>
  val y = x * 2
  val z = y + 1
  z
```

The final expression in a block is the return value. There is no `return` keyword.

## Function types

```lin
type BinaryOp = (Int32, Int32) => Int32
type Predicate<T> = (T) => Boolean
type Callback = () => Null
```

## Calling functions

```lin
val result = add(3, 4)
```

Arguments are evaluated left to right before the call.

## Dot application

`x.f(args)` is syntax sugar for `f(x, args)`:

```lin
val result = "hello".toUpper()   // toUpper("hello")
val r2 = [1,2,3].map(x => x * 2) // map([1,2,3], x => x * 2)
```

`x.f` without parentheses is partial application of `f` with `x` as the first argument:

```lin
val doubler = [1,2,3].map   // partially applied: map([1,2,3])
val doubled = doubler(x => x * 2)
```

## Partial application

Functions partially apply from left to right. Partial application is requested with an **explicit trailing comma** after the supplied arguments; the result is a new function awaiting the rest:

```lin
val add = (a: Int32, b: Int32) => a + b
val addTen = add(10,)      // (Int32) => Int32
val fifteen = addTen(5)    // 15
```

The trailing comma distinguishes partial application from a complete call. A call without it that supplies too few arguments is an error, unless the omitted trailing parameters have default values (§10.6), which are filled in instead:

```lin
val f = add(10)      // error: add has no default for `b`; use add(10,) to curry
val g = add(10,)     // partial application — g : (Int32) => Int32
val s = add(1, 2)    // complete call
```

Over-application (more arguments than the function expects) is a compile-time error.

## Recursion

A `val` whose right-hand side is a function literal may reference itself:

```lin
val factorial = (n: Int32): Int32 =>
  if n == 0 then 1
  else n * factorial(n - 1)
```

Mutual recursion between top-level `val` functions is also supported — both names are in scope in both bodies.

## Tail-call optimisation

Direct self-recursive calls in tail position are optimised to jumps. The following runs in constant stack space:

```lin
val sum = (n: Int32, acc: Int32): Int32 =>
  if n == 0 then acc
  else sum(n - 1, acc + n)
```

Mutual TCO is not performed in v1.

## Closure semantics

A function captures all bindings from its defining scope. `val` bindings are immutable copies; `var` bindings are captured by reference — all closures over the same `var` share the same mutable cell:

```lin
val makeCounter = () =>
  var count = 0
  {
    "increment": () => count = count + 1,
    "get": () => count
  }

val c = makeCounter()
c["increment"]()
c["increment"]()
c["get"]()    // 2
```

## `var` capture restrictions

`async` thunks (functions passed to `async()`) may not capture `var` bindings. This is a compile-time error where detectable. Workers may capture `var` bindings because they are single-threaded.

## Default parameters

A parameter may declare a default value. Optional (defaulted) parameters must come last — once a parameter has a default, every parameter after it must too. A default expression may reference parameters declared before it:

```lin
val box = (w: Int32, h: Int32 = w, area: Int32 = w * h) => area
box(4)        // h = 4, area = 16
box(4, 3)     // area = 12
```

A complete call must supply at least the required (non-defaulted) parameters; omitted trailing parameters are filled with their defaults. This is why a no-trailing-comma call like `add(10)` is only valid when the omitted parameters have defaults — otherwise use `add(10,)` to partially apply instead of filling.

## Parameter destructuring

```lin
val greet = ({ name, age }: Person): String =>
  "${name} is ${age}"
```
