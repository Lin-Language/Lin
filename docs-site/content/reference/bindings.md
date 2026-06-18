# Bindings & Scope

A binding associates a name with a value. Lin has two binding forms — `val` (immutable) and `var` (mutable) — plus destructuring forms that bind several names at once. This page also specifies Lin's scoping rules, including the **hard shadowing prohibition**.

For the surface syntax of declarations, see [Syntax](syntax.html); for module-level `export`/`import`, see [Modules](modules.html).

## Immutable bindings — `val`

`val` introduces an **immutable** binding. Once bound, the name cannot be reassigned.

```lin
val x = 1
val name: String = "Bob"
```

The type annotation is optional when it can be inferred from the right-hand side.

### A non-function `val` cannot self-reference

A `val` whose right-hand side is a **function literal** may reference itself by name — the name is in scope inside the function body, which is how recursion works:

```lin
val factorial = (n: Int32): Int32 =>
  if n == 0 then 1
  else n * factorial(n - 1)
```

A `val` whose right-hand side is **not** a function literal may **not** reference itself: `val n = n + 1` is a compile-time error, because `n` is not yet bound while its own initialiser is being evaluated.

Mutual recursion between two top-level `val` function literals is permitted — both names are in scope across both bodies, because top-level function `val`s are pre-scanned before evaluation.

## Mutable bindings — `var`

`var` introduces a **mutable** binding that can be reassigned with `=`:

```lin
var count = 0
count = count + 1
```

An assignment expression evaluates to the assigned value, so it can be the tail of a block or an `if` branch:

```lin
var count = 0
val result = count = count + 1   // result == 1; count == 1
```

The same holds for index assignment (`m[k] = v`) and field assignment (`rec["f"] = v`) — each evaluates to the stored value.

### `var` is captured by reference

This is the most important non-obvious property of `var`. A closure that captures a `var` captures the **mutable storage cell**, not a snapshot of its value. Two closures that capture the same `var` share **one** underlying cell — a write through one is visible through the other.

```lin
type Counter = { "inc": (() => Int32), "get": (() => Int32) }

val makeCounter = (): Counter =>
  var count = 0
  {
    "inc": () =>
      count = count + 1
      count,
    "get": () => count
  }
```

```lin
import { print } from "std/io"
import { toString } from "std/string"

val c = makeCounter()
print(toString(c["inc"]()))   // 1
print(toString(c["inc"]()))   // 2
print(toString(c["get"]()))   // 2  — same cell, not a snapshot
```

The implication for loop-like code: if you build several closures inside a `.for(…)` that each capture the same enclosing `var`, they do **not** each get a per-iteration snapshot — they all observe the cell's *final* value. When you need a distinct value per element, take it as the lambda's parameter (which is a fresh binding each call) rather than reading a shared outer `var`.

(Note: an `async` thunk may **not** capture a `var` — that is a compile-time error, precisely because the cell is shared mutable state crossing a thread boundary. See the concurrency docs.)

## Destructuring bindings

A `val` (or `var`) may bind several names at once by destructuring an object or array. The same patterns are available in function parameters, `match` arms, and imports.

### Object destructuring

```lin
type Person = { "name": String, "age": Int32 }

val describe = (person: Person): String =>
  val { name, age } = person
  "${name} is ${age}"
```

A bare name (`name`) is shorthand for the quoted key with the same local name — `{ name }` means `{ "name": name }`. (This shorthand applies only to *patterns*; object **literals** always require quoted keys.) To bind a field to a different local name, write the explicit form:

```lin
val displayNameOf = (person: Person): String =>
  val { "name": displayName } = person
  displayName
```

### Array destructuring

```lin
val labelOf = (pair: [String, Int32]): String =>
  val [first, second] = pair
  "${first}=${second}"
```

### Rest with `...`

An array pattern may end in a rest binding that captures the remaining elements as an array:

```lin
val tailOf = (xs: Int32[]): Int32[] =>
  val [head, ...rest] = xs
  rest
```

An object pattern may end in an object rest that captures the remaining fields:

```lin
val withoutName = (person: Person): String =>
  val { name, ...remaining } = person
  "${name}: ${remaining["age"]}"
```

Destructuring patterns also nest — an object pattern may contain a nested object or array pattern, to any depth.

## Scope

### Lexical block scope

Bindings are **lexically scoped** to the block in which they appear. A binding introduced inside a function body, an `if`/`match` branch, or a lambda is visible only from its point of declaration to the end of that block, and is not visible outside it.

A block evaluates to its final expression, and earlier `val`/`var` bindings in the block are visible to the expressions that follow them.

### Module-level bindings

Top-level bindings in a file form the module scope. They are private to the module unless marked `export`. Module code runs top-to-bottom on first import, so a `val` must generally appear before code that uses it — with the exception of mutually-recursive top-level function `val`s, which are pre-scanned. See [Modules](modules.html).

## Shadowing is a compile-time error

Lin **forbids inner-scope shadowing**. Reusing a name that is already visible from an enclosing scope, in a strictly inner scope, is a **hard compile-time error**:

```
<name> shadows a binding from an enclosing scope
```

The rule prevents a common class of bug: adding an import or an outer `val`, then silently changing which value an inner binding refers to.

### What is rejected

The prohibition applies to every inner binding site:

- inner `val` / `var` bindings,
- function and lambda **parameters**,
- destructuring captures (object-field captures, rest bindings, and `match`-arm captures).

```lin
val x = 1
val f = (): Int32 =>
  val x = 2   // Error: `x` shadows a binding from an enclosing scope
  x
```

The compiler points at both the inner (rejected) binding and the outer one it shadows, and suggests renaming the inner binding. The outer/imported binding is **never** the thing you change.

### What is allowed

Three things are **not** shadowing and remain legal:

**1. Same-scope sequential rebinding.** Two `val` (or `var`) statements with the same name in the *same* block are not inner-shadows-outer — the second simply rebinds at the same depth:

```lin
val sameScopeOk = (): Int32 =>
  val x = 1
  val x = x + 1   // OK — same scope, sequential rebinding
  x
```

**2. Sibling-scope reuse.** Two adjacent lambdas each open a fresh scope at the *same* nesting depth, so neither shadows the other. This is why the ubiquitous chained-combinator idiom — the same parameter name in adjacent lambdas — is fine:

```lin
import { map, filter } from "std/iter"

val pipeline = (xs: Int32[]): Int32[] =>
  xs.map(x => x * 2).filter(x => x > 2)   // OK — sibling lambdas, both name the param `x`
```

**3. The wildcard `_`** (and compiler-generated synthetic names) are exempt. Use `_` for a throwaway binding — for example, ignoring the index parameter that combinators pass:

```lin
import { map } from "std/iter"

val doubled = (xs: Int32[]): Int32[] =>
  xs.map((value, _) => value * 2)
```

A function body that references its own pre-scanned recursive slot is likewise exempt — that is recursion, not shadowing.
