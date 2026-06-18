# Generics

Generics let a type or function be written once and reused at many concrete types without losing precision. Lin generics are **monomorphized** — each instantiation is compiled to its own specialised native code, so a generic function is as fast as the hand-written concrete version. There is no boxing tax for being generic.

This page covers generic type declarations, generic functions, how type arguments are inferred, variance, and how generic values are matched. For the broader type system, see [Types](types.html).

## Generic type parameters

A type parameter is written in angle brackets after the name being declared. Both **declaration** and **application** use angle brackets:

```lin
type Box<T> = { "value": T, "label": String }

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

type Mapper<T, U> = (T) => U

// Application: supply concrete types for the parameters.
type ParseResult = Result<Int32, String>
```

A generic type may take any number of parameters, and they may appear anywhere a type can: in a field type, an array element, a union member, or a function arrow.

## Generic functions

A `val` function may declare type parameters **before** its argument list. The parameters are then available in the parameter types, the return type, and the body:

```lin
val identity = <T>(x: T): T => x
val firstOf = <T>(xs: T[]): T => xs[0]
val pair = <A, B>(a: A, b: B): { "first": A, "second": B } =>
  { "first": a, "second": b }
```

A generic body type-checks against the *bound* the parameters give it — `T` is an opaque type with no operations beyond what the parameter types allow — so the same compiled logic is valid for every instantiation.

## Type-argument inference is argument-driven

Type arguments are **never written explicitly at the call site**. There is no turbofish syntax — `identity<Int32>(42)` does not work (it parses as the comparison `identity < Int32 > (42)`, and `Int32` is then an undefined variable). Instead, the compiler infers each type parameter from the **types of the arguments** at the call site:

```lin
val a = identity(42)        // T = Int32  → a : Int32
val b = identity("hi")      // T = String → b : String
val c = firstOf([1, 2, 3])  // T = Int32  → c : Int32
val d = firstOf(["a"])      // T = String → d : String
```

The type parameter ties the result to the argument: `firstOf([1, 2, 3])` returns an `Int32` because the element type flowed into `T`.

### Limits of argument-driven inference

Because inference flows **only from arguments**, a type parameter that does not appear in any parameter type cannot be inferred from a call:

- A **zero-argument** generic function has nothing to infer from. `val makeEmpty = <T>(): T[] => []` is legal, but a bare call `makeEmpty()` leaves `T` unconstrained at the call site; `T` is only pinned later if the result flows into a context that fixes it (for example, an annotated binding or a subsequent operation).
- A **return-only** type parameter (one that appears only in the return type, not in any argument) is in the same situation — there is no argument to infer it from.

The practical rule: make every type parameter you need inferred appear in at least one **argument** position. If a value's element type must be known, take an argument of that type (or a `T[]`, `(T) => U`, etc.) rather than relying on the return type alone.

## Variance

Generic types and function types are **variant** according to where the parameter appears:

- **Covariant in producer positions** — return type, array element, container content. A more specific producer is assignable to a less specific one.
- **Contravariant in consumer positions** — function arguments. A *more general* consumer is assignable where a *more specific* one is expected.

Covariance (producer):

```lin
type Person = { "name": String }

val people: Person[] = [{ "name": "A" }, { "name": "B" }]
val anything: AnyVal[] = people        // Person[] assignable to AnyVal[]
```

Contravariance (consumer) — a function that consumes the *wider* `AnyVal` can stand in where a `Person` consumer is required, because it safely handles every `Person`:

```lin
val takesPersonConsumer = (g: (Person) => Int32): Int32 => g({ "name": "x" })

val anyConsumer = (v: AnyVal): Int32 => 1
val ok = takesPersonConsumer(anyConsumer)   // (AnyVal) => Int32 accepted for (Person) => Int32
```

The reverse is rejected: a `(Person) => Int32` is **not** assignable where `(AnyVal) => Int32` is expected, because a `Person`-consumer cannot safely handle an arbitrary `AnyVal` argument.

In summary: `Person[]` → `AnyVal[]` (covariant element), `Iterator<Person>` → `Iterator<AnyVal>`, a function *returning* `Person` → one returning `AnyVal`, but `(AnyVal) => U` → `(Person) => U` (contravariant argument).

## Monomorphization

Each distinct instantiation of a generic is compiled to its **own specialised copy** of the native code, with the concrete types substituted in. `identity(42)` and `identity("hi")` compile to two specialisations; there is no shared boxed code path and no runtime type dispatch. A generic function therefore costs exactly what the equivalent concrete function costs — generics are **zero-cost**.

The built-in numerically-bounded parameter `Number` works the same way: a `(x: Number)` function is sugar for an implicit numeric type parameter, and each call site monomorphizes a specialisation for the concrete numeric family that flows in (`$Int32`, `$Float64`, …), each emitting native unboxed arithmetic. See [Types](types.html) for `Number`.

### Across module boundaries

Generic type and function declarations work across module boundaries. A cross-module generic function is **monomorphized per importer** — each importing module compiles the specialisations it actually uses. Exported generic types (e.g. `Result<T, E>`) are imported and applied like any other type. See [Modules](modules.html).

## Matching generic values

A generic **application cannot appear in an `is` pattern**. `is Result<Int32, String>` is not supported — at runtime there is no generic type to test against, only the underlying tagged shape. (It does not even parse as a pattern.)

To match a generic value, match its underlying **tagged shape** with `has` (presence) arms, narrowing on the string-literal discriminant:

```lin
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

val unwrapOr = <T, E>(r: Result<T, E>, fallback: T): T =>
  match r
    has { "type": "success", value } => value
    else => fallback

val ok: Result<Int32, String> = { "type": "success", "value": 42 }
val x = unwrapOr(ok, 0)
```

This is the reason union variants are conventionally given **string-literal discriminants** (`"type": "success"`): they let a `match` over a generic union narrow cleanly without naming the generic application. See [Pattern Matching](pattern-matching.html).
